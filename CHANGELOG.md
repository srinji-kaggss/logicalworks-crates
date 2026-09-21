# Changelog

All notable changes to the four crates are recorded here. Versions move
independently; each release lists per-crate deltas. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
`0.x`, so any minor may carry breaking changes, which are then listed
explicitly under that crate.

## [Unreleased]

## [lgwks_std 0.6.7 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13] - 2026-09-21

### Proofs

- **`proofs/bot-spec.sml` states and proves seven theorems about the
  spec/condition layer**, machine-checked by the HOL4 kernel: totality of the
  loop-free evaluator (`T1`), budget monotonicity (`T2_MONO`), budgeted-verdict
  agreement (`T3`), **capability-gate soundness** — every entry that fires is one
  the grant permits (`T4`) — gate monotonicity in the grant (`T5`), and
  **record/replay agreement** (`T6`). `T7` proves that the loop constructor
  admits no value at any finite budget, which is the formal reason `T1`'s
  totality is a property of the loop-free language and does not survive the
  extension. `proofs/proof-run.log` is the unedited run; no `mk_thm`,
  `new_axiom` or `cheat` appears anywhere in the script.
- **It is not a refinement proof, and the README says so first.** The theorems
  are about the script's own hand-written datatypes, not the Rust; nothing here
  proves that `spec.rs` implements the model. The claim is therefore split: the
  *design* has these properties (proved), and the *implementation* cannot express
  the states that would break them (argued from type construction — a private
  `Auth` constructor, a `Cap` newtype, a grant written only in `assemble`). The
  model's abstractions are enumerated too: `Contains` is an opaque literal,
  `Env` is total, effects are not modelled, capability shortfall is not modelled,
  and the runtime is absent entirely. A bug in the model is a bug in what is
  proved.

### Benchmarks

- **`bench/` measures `lgwks_bot` against a hand-rolled baseline doing identical
  work**, gated on the bot and the baseline agreeing on both effect counts and
  condition-evaluation counts. That second axis is load-bearing: gating on
  effects alone let a baseline that returned `entries` as an integer instead of
  looping over them report a 2937x ratio where the counted-work gate measures
  72.38x at the same scenario. The rig is its own Cargo workspace root so the
  dependency contract never sees it — `lgwks-deps check .` still reports the four
  crates.
- **The measured result is that the bot is 72x–256x slower than the loop, and
  ~98% of a tick is the poll and change-detection phase before any decision is
  made** (poll-only 2540.0 ns/tick against steady 2582.1 ns/tick at 64 chains).
  Source cost is linear; change detection now pays 4.23x, because both changes
  below made a quiet tick cheaper without making a churning one cheaper, so a
  workload that never goes quiet pays about four times what a quiet one does.
- **The rig found a defect, and the fix it was measured against has landed.**
  `Auth::check` grew 3794x for a 128x increase in required capabilities, reaching
  12.9 µs per call at 128 capabilities, because coverage was a linear scan of
  granted names per required name. The sorted-and-deduplicated proof with a
  binary search replaced it, and the same rig measures growth of 750.5x and
  3.64 µs at 128. The bench directory measures and does not touch crate code.
- **These are the figures for the tip of this release, and every one is paired
  within a run.** `bench/README.md` carries the method, the drift control and the
  attribution between the two changes; `bench/results.json` is the committed run
  they come from.

### Decision

- **`lgwks_bot` is relicensed to MPL-2.0**, from Apache-2.0. The bot is the
  artefact the rest of the estate embeds, and a permissive licence on it lets
  anyone modify it and ship the modifications closed with no later release able
  to recover that. MPL-2.0 is file-level copyleft: use stays unrestricted
  including in proprietary products (MPL-2.0 §3.3), and only modification of the
  MPL-covered files carries the §3.2 source obligation. `lgwks_std`, `lgwks_ast`
  and `lgwks_deps` remain Apache-2.0.
- **No published version changed, and the new terms land at `lgwks_bot` 0.5.0.**
  Licences are not retroactive; every version on crates.io including `lgwks_bot`
  0.4.2 stays Apache-2.0. The release this section is part of is the first cut
  from this tree under MPL-2.0, so 0.5.0 is the version the terms take effect at.
  The four dependent repositories pin exact published versions and none is
  affected until it moves.
- **The repository root now carries the MPL-2.0 text.** There was no root
  `LICENSE`, so the repository's own licence was unstated while the bot's was
  not, and a reader arriving at the tree rather than at a crate had nothing to
  read. The root file covers the documentation, the scripts and the CI
  configuration; each crate is still governed by the `LICENSE` in its own
  directory, so `lgwks_std`, `lgwks_ast` and `lgwks_deps` remain Apache-2.0 as
  consumed artefacts. `LICENSING.md` states which file governs what.
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

### lgwks_std Added

- **`wire` re-exports the `rkyv` crate, so a consumer crate can derive against
  it.** The module re-exported the three derive macros but not the crate those
  macros expand against, and the generated code names `::rkyv::…` absolutely. A
  type outside `lgwks_std` that derived `Archive` through this module therefore
  failed with `cannot find 'rkyv' in the crate root` before it ever reached a
  layout question, which made the estate's binary surface usable only by its own
  tests. A consumer now writes `#[rkyv(crate = lgwks_std::wire::rkyv)]` and
  derives the macros from here as before.
- **`Digest` carries the archive derives behind `wire`.** Thirty-two bytes with
  no indirection, so it archives in place and any record that contains one
  reaches it without a pointer — which is the property that makes the journal's
  chain head a value a reader can compare against without decoding the record.


- **`ron::Error` and `ron::error::SpannedError` are re-exported.** The `ron`
  module's own functions returned them and no caller could name them, because
  the `ron` crate is an optional dependency and nothing re-exported the types.
  A facade that returns a type has to let a caller write it down, which is the
  same reason `wire` re-exports `rkyv`. Additive.

### lgwks_bot Breaking

**There is no longer any public API that starts concurrent work or a process and
hands the caller something droppable.** A `JoinHandle` a caller can drop is a
task whose outcome nobody ever sees; a `Child` is the same shape with a
process attached. Both were reachable, and both are gone. The only way to start
concurrent work is `Supervisor`, and the only way to start a process is a
supervised one. Nothing is weakened to pay for it — the guarantees move from
convention to the type system, because the constructors are gone.

Removed, each with what replaced it:

- `rt::task::spawn`, `rt::task::spawn_local`, `rt::task::spawn_blocking` — the
  three functions that returned a droppable `JoinHandle`. Fan-out is
  `rt::task::join_all_bounded` (bounded, ordered) or a `JoinSet` the caller
  owns; what outlives the call belongs in a `Supervisor`. `spawn_local`'s
  re-export of `LocalSet` went with it: a local set whose tasks could be
  detached is the same hole one thread down.
- The `rt::task` re-exports of `JoinHandle` and `LocalSet` — nothing public
  returns either any more. `JoinSet`, `JoinError`, and `AbortHandle` remain:
  those are the tracked envelope and the results it yields.
- `rt::runtime::Handle::spawn` — the same droppable handle, reached through the
  runtime instead of the module. This one was not in the original scope and is
  removed because the stated principle decides it: a handle that cannot start
  work but can *drive* it (`Handle::block_on`) still covers every legitimate use.
- `rt::process::{Child, ChildStdin, ChildStdout, ChildStderr}` — a `Child` is a
  handle to a running process, and `Child::kill` reaches neither a shell's
  grandchildren nor a process whose handle was dropped. `Command` stays public:
  a caller must still be able to describe what to run.

Added:

- `Supervisor::spawn_process(&mut Command) -> io::Result<TaskId>`. Takes a
  command, awaits an in-flight permit exactly as `spawn` does, and returns no
  handle — a `TaskId`, which cannot join, abort, or wait. The task it places is
  the process's only owner: the child is put in its own process group and the
  **group** is killed when the task is cancelled or the supervisor drops, so a
  shell cannot leave grandchildren behind. This is what `contract/APPROVED.toml`
  records `rustix` under `lgwks_std` for. A command that cannot start returns
  the `io::Error` to the caller without consuming a slot.
- `Supervisor::default()`. The ceiling is discovered from
  `std::thread::available_parallelism` instead of required, so the safe
  constructor is the one that resolves first; `Supervisor::new(max_in_flight)` is
  unchanged for callers with an opinion. Bounded either way.
- `TaskOutcome::Failed { task, status }`, `TaskOutcome::is_process_failure`,
  `TaskOutcome::exit_status`, `Stats::failed`, and `ShutdownReport::failed`. A
  process that exited non-zero, died from a signal, or whose status could not be
  read is a *failure* carrying its `ExitStatus` — not a completion, and not the
  same report as a kill this supervisor ordered. Without this the three cases
  were one increment, and a bot running a failing command read as a healthy one.
- `clippy.toml` bans `tokio::process::Command::spawn` with
  `Supervisor::spawn_process` as its named replacement. `Command` is the
  engine's own type, so its methods cannot be narrowed, and this is the one
  remaining path that is closed by a lint rather than by the type system — named
  here rather than left to be discovered.

Changed:

- The `process` feature now implies `sync`, its `io`, `lgwks_std/process`, and
  `lgwks_deps/tokio-process` edges unchanged. A `process` build without `sync`
  would expose `Command` and not the runner, leaving the banned `Command::spawn`
  as the only way to use it. No dependency was added: `rustix` stays
  `lgwks_std`'s, under its recorded `allowed_consumers`.
- `Supervisor::shutdown`'s grace before the abort is now a **wall-clock** bound
  (50 ms) rather than eight `yield_now` calls. A counted grace is a same-thread
  heuristic: `yield_now` reschedules the yielding task, so on a multi-threaded
  runtime it gives a body parked on another worker no chance to observe its
  cancellation, and a cancelled task was reported as `Aborted`. Found by this
  change's own process tests, which could not tell a killed command from a
  failed one because of it. A body that returns is settled the moment it does,
  so this is a ceiling on the wait, not a cost charged to every shutdown.
- `rt::task` no longer re-exports anything that starts work; `rt::process`
  exports `Command` and a module doc explaining the one path that is a lint
  rather than a type-level removal.
- `docs/guides/lgwks-bot/background-work.md`, `crates/lgwks-bot/README.md`, and
  the `rt::task` / `rt::process` module docs now describe the supervised-only
  surface rather than the deleted one.
- **`Bot::resolve_effect` takes the `PendingWork` it is about rather than
  `(WorkId, revision)`.** `PendingWork` carries the address, the generation and
  the attempt, and all three are compared before anything is read or written. A
  caller passes back the value `pending()` handed it, so "settle an attempt I
  invented" and "settle the wrong attempt" stop being representable rather than
  being refused after the fact. `WorkId` remains as the address a report names,
  which is what it always was: SPEC-02's "a slot index is instrumentation only".
  This is the intentional tightening RQ-059 asks to be documented — a program
  that named a slot and a revision still compiles if it reads its work from
  `pending()`, and a program that fabricated the pair no longer does.

- **`EffectEvent::to_bytes` returns `lgwks_std::wire::AlignedVec` rather than
  `Vec<u8>`.** The journal record is archived by the estate's binary format now
  instead of a framing this crate maintained beside it, and an `AlignedVec` is
  what the archive has to live in for a reader to access it in place rather than
  decode it. The tag bytes, the width constants and the hand-written
  `encode_into` are gone with it. **The bytes are what a chain head commits to**,
  so this is a durable break as well as a signature one: a journal written before
  this release does not verify against one written after it.

**No crate version is bumped.** The change is unpublished like the five public
modules above it, and it ships in the same release.

### lgwks_bot Added

- **The `domain_id -> constructor` registry exists.** A `BotSpec` carries
  identifiers rather than code, and something has to turn `"github::pr_status"`
  back into a running `Observe`. `DomainRegistry` is that list, and `domains!`
  is the one entry point that declares it:
  ```rust
  domains! {
      pub DOMAINS {
          observe { "github::pr_status" => GithubPrStatus::from_target }
          execute { "notify::slack"     => SlackNotify::from_target }
      }
  }
  ```
  The list is a `static` built at compile time — no constructor to call, no
  global to mutate, so two components cannot race to register a domain and the
  set a binary can run is readable from its source. `Source` and `Action` are
  the erased handles a constructor returns, and `build_source`/`build_action`
  refuse an unlisted identifier with `BotError::UnregisteredDomain`, escaped
  because the name came from the document.
  - **A declarative list, not a proc macro.** The mapping is data a reader can
    see whole. `domains!` is `macro_rules!`, so this adds no dependency and
    leaves the repo's stated position on the `syn` stack untouched —
    `lgwks-std/Cargo.toml` records that policy, and it still holds. A `bot!`
    proc-macro remains deliberately absent for the second reason it always had:
    it would hide the per-call `Auth::check` that auditors read.
  - **The registry is not a permission.** It answers which constructor an
    identifier names, never what a bot may reach. Authority still comes from the
    caller-held `GrantSet`, so a spec cannot grant itself anything by naming a
    domain.
  - **`from_spec` is still open, and now for stated reasons.** It is not
    plumbing: `ChainSpec::on` is `Vec<(String, ActionSpec)>`, so a condition is a
    bare identifier with no slot for `Above<u16>`'s threshold, and an erased
    `Box<dyn ObserveAny>` carries no serializable statement of its `Output` type
    (`spec::Witness` is a `TypeId`, process-local). Both are recorded in
    `experience/invariants/sdk.yaml`, which gains a `registry` lane.
  - `crates/lgwks-bot/tests/registry.rs` enforces the above, including that a
    source identifier never resolves as an action, and
    `docs/guides/lgwks-bot/domains.md` is the guide.

- **A flow can be written in RON, as well as JSON.** `FlowSpec::from_ron` and
  `FlowSpec::to_ron` are the codec, over the same serde types, so no second set
  of impls exists. RON is the estate's notation for human-facing documents and a
  flow is one, so a hand-written flow now takes comments, trailing commas and
  unquoted keys.
  - **The two notations are one document.** Both parsers converge on one
    validation, so the `MAX_FLOW_BYTES` size limit, the
    `BotError::UnknownNodeKind` diagnostic and the structural checks are
    identical on either path. A document cannot be acceptable in one notation and
    refused in the other.
  - **A variant is spelled with its own name.** Every enum in a flow document is
    externally tagged, so a unit variant is its own name (`end`) and a variant
    carrying fields takes them in parentheses (`say(text: "hello")`); in JSON the
    same two are the bare string `"end"` and `{"say": {"text": "hello"}}`. This
    is a change to the wire spelling rather than to the notation: the seven enums
    a document carries (`NodeKind`, `FlowEdge`, `Terminal`, `Predicate`,
    `ValueExpr`, `Value`, `VarType`) previously took a `kind` field, and a
    document written in the older spelling is now refused rather than
    reinterpreted.
  - **The unknown-kind diagnostic reads differently on each path.** JSON checks a
    variant name against nothing, so `BotError::UnknownNodeKind` names the node
    and the kind it carried; RON checks the name against the variant list it is
    handed and refuses before our own visitor runs, so its refusal names the
    variant and the enum but cannot name the node. Both refuse; only what each
    can say differs.
  - `crates/lgwks-bot/tests/flow_ron.rs` pins all of the above, including that
    the internally tagged spelling no longer decodes.

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
- **`Observe::fingerprint`, and with it the lazy seam: a source that holds still
  is no longer polled.** Change detection is an *equality* question — the
  substrate reduces every observation to one bit and discards the value — so
  making a source produce the value in order to ask the bit is the eager part of
  the loop. `fn fingerprint(&self) -> Option<u128>` lets a source answer the
  equality question from a key it already holds (an `ETag`, an `mtime`, a row
  version, a mutation counter, a hash of a framebuffer), and the tick then
  **never calls `poll`**: no value is constructed, nothing is boxed, and no
  future is built. The decision is taken *before* the future exists — a check
  inside the future would still have allocated the box carrying it, which on a
  quiet tick is the only thing there was to allocate.
  - **The contract is exact: equal fingerprints must imply equal values.** A
    digest that collides across two different values makes the substrate skip a
    real movement, which is a silent no-op rather than a wrong effect — the one
    failure this can introduce. The digest is recorded at the moment it is read,
    *before* the poll, so a source that moves in between leaves a digest that no
    longer matches and the next tick polls again: the error is one redundant
    poll, never a missed movement. A poll that fails clears the digest, so a
    source that errors still reports every tick, exactly as before.
  - **The default is `None`, which is today's path exactly** — every source
    written before this method existed keeps its behaviour and its cost, and
    there is no configuration in which a domain silently loses an observation.
  - Measured, `bench/` A/B/A in one session, seam versus the tick-allocations
    change beneath it: `poll-only-64x100` **1.65x faster**, `steady-64x100`
    **1.68x**, `wide-256x10` **1.46x**. `fanout-1x64` and `churn-64x1` are
    unchanged: their moves (1.8% and 7.6%) are smaller than the control's own
    drift at the same scenario (6.9% and 10.8%). Allocations per tick:
    **167.6 -> 22.9** steady, **171.0 -> 21.4** poll-only, **650.9 -> 141.7**
    wide. Against the base before either change: **2.71x**, **1.99x** and
    **1.66x**.
  - The rig's own source implements it as the identity (`u128::from(value)`),
    deliberately: a real source would use a key cheaper than the value, so the
    reported gain is a **floor** and not a best case.
  - Three tests, each verified to fail against the seam removed: a quiet source
    is polled once over twenty-one ticks (it reports 21 without the seam), a
    digest that moves is polled again and fires, and a source with no digest is
    polled every tick exactly as before.
- **The tick no longer allocates its own bookkeeping, and a source that holds
  still is no longer boxed.** A steady-state tick at 64 chains made **171 heap
  allocations**; it now makes **97**, and the tick is up to **1.6x faster**.
  Four changes, all measured by the `bench/` rig rather than argued:
  - `fire_plan` cleared the plan and then assigned it freshly collected `Vec`s,
    so the `clear` bought nothing and every tick paid two allocations per chain
    to rebuild buffers it was about to overwrite. The per-tick scratch
    (`Plan::steps`, `Plan::failures`, `Polled`, `Observed`, `Moving`, `Moved`)
    is now taken, cleared and handed back, and the intermediate `Vec<usize>` of
    moved chains and the per-tick `Order::clone()` are gone.
  - **`ObserveAny::poll_any` takes the payload the substrate currently holds for
    its chain and returns `Ok(None)` when the source produces an equal value.**
    The comparison now happens where the output is still a concrete
    `T::Output`, which is the only place it can happen without boxing: a
    caller-side `==` on two `Box<dyn Any>` costs an allocation, a memcpy and a
    free apiece, spent to answer a question that could be asked of the value in
    hand — and in a steady-state bot the answer is "equal" on nearly every tick.
    The blanket `impl ObserveAny` therefore carries `T::Output: PartialEq`,
    which is not a new requirement: every chain is closed by
    `EcsObserveBuilder::observe`, which already demands it.
  - **A source whose equality is narrower than its identity now holds the
    *earlier* of two equal values.** This is a real semantic tightening and it
    is the one to read carefully. `Observed` keeps the payload it already has
    when the source reports equality, rather than being overwritten with the
    newer-but-equal instance. Nothing an effect observes changes — a retained
    transition already refuses to let an equal-valued observation displace its
    binding, so the action was receiving the older instance anyway. What changes
    is a caller reading the observation back after an equal-valued tick. A type
    whose `PartialEq` ignores a field it nevertheless carries should not be a
    chain's output type.
  - The measurement, and the per-tick allocation trace that gives it, are
    `bench/`'s `--alloc-report` mode, which counts through a forwarding
    `#[global_allocator]` in that rig only. It is a diagnostic: no timing in
    `results.json` is taken from a counting run, and no `unsafe` reaches
    `lgwks_bot` or `lgwks_std`.
- **`ObserveBuilder` is now generic over its source, and a chain that does not
  type against that source no longer builds.** `EcsObserveBuilder::on` tied the
  condition to a *free* type parameter connected to nothing else, so the builder
  accepted a wiring that could not work and the tick found out instead: a `u16`
  source, a `u16` condition, and an action whose `Execute::Input` was `u32`
  compiled, ran, and failed every attempt as a domain error the retry policy
  then retried. `on<C, A, T>` is now `on<C, A>` with `C: Evaluate<S::Output>`
  and `A: Execute<Input = S::Output>`, and the builder holds `source: S` until
  the erasure boundary. **This is a deliberate source-breaking tightening**, not
  an accident of the rewrite:
  `Bot::builder("x").observe(source).on(condition, action)` now fails to compile
  when `condition`'s `Evaluate<T>` is not `Evaluate<S::Output>` or `action`'s
  `Execute::Input` is not `S::Output`. Both rejected shapes only ever failed at
  tick time, so nothing that worked stops working — but code that compiled and
  misbehaved now does not compile at all, and a caller who was relying on the
  old looseness must fix the wiring rather than the budget. `observe`/`build`
  remain the erasure boundary and still box into `Box<dyn ObserveAny>`. Sound
  because `Evaluate::check` returns `bool` — a pure predicate with no derived
  output — so `Source::Output == Condition::Input == Action::Input` is already
  the real semantics. The rule a future verb has to keep: no stage may introduce
  a caller-selected type parameter disconnected from its input; a transform must
  carry `Transform<I>::Output` as an associated type. Every `.on` call site in
  the estate already annotated its closure or named its condition type, so no
  call site needed a turbofish added.
- **The erasure boundary carries a witness, and a mis-pairing across it is a
  loud invariant violation rather than a domain error.** `Observed` was a
  `Vec<Option<Box<dyn Any>>>` paired to `Chains` by *index*, and nothing about
  that pairing was checked, so a value delivered to the wrong chain produced a
  downcast miss reported as a domain failure — which the retry policy then
  retried. The staging slot now holds an `Erased` — the boxed value and the
  `TypeId` witness of the type it was erased from, one allocation rather than
  two — and the witness is compared at the rendezvous before any downcast. A
  miss is `BotError::TypeMismatch`, naming the site, the chain, and both types.
  The witness rides with the value rather than only on the chain, so the
  comparison is a producer's *claim* against a consumer's *expectation* rather
  than ground truth against an expectation. `TypeId` is process-local and not
  serializable, so the witness serves the Rust path only; the durable schema key
  belongs to the registry, and `Erased::witness` is the field it will land in.
  One limitation is documented and not solved here: the witness distinguishes
  *types*, not *chains*, so two chains that both produce a `u16` are
  indistinguishable and a mis-pairing between them still passes.


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

### lgwks_bot Fixed

- **A settlement is about one attempt, and a repeat of it can no longer land on
  the next one.** The ledger guarded the slot and the generation but not the
  attempt, and a chain retries *within* a generation: an entry held as an
  unknown outcome is settled `NotApplied`, that makes it eligible, and the next
  tick begins attempt 2 at the same address under the same revision. A repeat of
  the attempt-1 report — which settlement tolerates on purpose, because a caller
  that never saw its first delivery acknowledged has to be able to send it again
  — then found the entry held, passed every check, and recorded "definitely did
  not happen" about attempt 2 on the strength of a statement about attempt 1.
  Attempt 3 ran against an effect that may have been live. The ledger now
  numbers the attempts it begins and refuses evidence whose attempt is not the
  outstanding one (`BotError::EvidenceStaleAttempt`), and the number is handed
  back by the ledger rather than derived beside it, so a settled entry cannot
  restart the count at one and give two attempts the same name.
  `tests/ecs_tick.rs::a_settlement_names_the_attempt_it_settles_and_no_other`
  fails with the guard removed and passes with it.
- **A transition that is dropped hands its payload back to the chain that owned
  it, so a settled chain settles.** Found by `bench/`'s fairness gate and by
  nothing else, which is the point: **the bot fired 896,000 effects where the
  hand-rolled baseline fired 17,920 for identical input — a 50x over-run — and
  every one of the crate's 611 tests passed straight through it.** A transition
  *takes* the observed value out of its slot, and while the transition is
  retained that binding is where the chain's newest value lives; when the
  transition finished and was dropped, the value went with it and the chain was
  left with no baseline at all. The next tick read "no baseline" as "the source
  moved", re-opened the chain and fired the entry again, every tick, forever.
  The binding now goes back into the empty slot — only into an empty one, since
  a slot the observation phase filled this tick holds something newer.
  Regression: `ecs::tests::a_settled_chain_does_not_fire_again_on_every_later_tick`,
  which fails against the defect with `tick 3: left: 1, right: 0` and passes
  after it. It uses a new `Holds` fixture rather than the existing `Script`,
  because a source that advances on every poll cannot tell a chain that settled
  from a chain that is still working — which is the reason the existing
  change-filter tests did not catch this.
- **A transition is bound to the observed payload it was opened under, and a
  newer value is admitted only once it has nothing open.** Conditions were
  evaluated and actions were run against the *newest* observation, while a
  resumed transition kept the revision and the entries of the old one. A chain
  whose first action succeeded and whose second refused under one input would
  retry that second action under the *next* input, so one transition produced
  effects of two different command inputs and reported itself as a single
  finished unit of work — the acknowledgment of the first combined with an
  effect of the second. The observation is now *moved* out of the fold's slot
  and into the transition when it opens or resumes, every entry of that
  transition reads the binding, and the slot stays empty for as long as the
  binding is out on loan. It is moved rather than copied, so nothing here asks a
  source's `Output` for `Clone`: `EcsObserveBuilder::observe` requires
  `PartialEq + 'static` exactly as before. A movement that arrives mid-transition
  is deferred, not dropped — it is still in the slot, and it becomes the next
  transition's payload on the first tick after the current one drains. The
  ledger becomes a non-send resource as a consequence, because the payload
  travels inside the transition rather than in a second index that every
  `take`, `put`, `begin`, `skip` and `fail` would have to keep in step.
- **A failure now carries what it establishes, not only what went wrong.**
  `BotError::DomainError` was the adapter's catch-all, and `failure_state`
  classified from the variant — so it was "retryable" by construction. A
  permanent refusal and a wiring defect both burned the whole
  `RetryPolicy::DEFAULT` budget and were reported as `AttemptsExhausted` ("we
  ran out of budget") when the truth was `Terminal`. `DomainError` carries a
  required `certainty: DispatchCertainty` (`Refused` / `NotDelivered` /
  `Unsettled`), `BotError::retry_class()` is the single total map to `RetryClass`
  (`Never` / `Safe` / `RequiresEvidence`) that never inspects a rendered cause,
  and `failure_state` reads that and nothing else. A wiring defect or a parse
  refusal is `Terminal` and costs exactly one attempt whatever the budget says.
  Every one of the 16 production `DomainError` construction sites was classified
  and the classification is now required by the type, so a new producer cannot
  omit it: 15 are `Refused`, and the `JsonStore` local read in `domain/data.rs`
  is `NotDelivered` — a read that never left the process is the one failure here
  that is safe to repeat.
- **An abandoned entry is a barrier to its successors, and a tick over one is
  never clean.** `plan_chain` treated `Abandoned` like `Succeeded` and walked
  past it, so the entry behind a prerequisite that had been given up on ran
  anyway: reserve a draft, fail the reserve under `RetryPolicy::ONE_ATTEMPT`, and
  the next tick sent the draft that was never reserved. Declaration order is a
  prerequisite chain, not a list of independent steps, and abandonment is the
  strongest statement that the entry behind it must not run. The walk now stops
  there; successors stay `NotStarted` and stay reported, and evidence
  (`EffectEvidence::NotApplied`) remains the way past, reviving the entry and
  them with it.
- `EntryState::is_open` excludes `Abandoned`, so the tick's completion check — a
  scan for the first open entry — walked past an abandonment too and returned
  `Ok(0)` while `Bot::pending()` still named it. A caller reading a clean `Ok` as
  "this chain is handled" read the opposite of what the ledger held. The check is
  now `Ledger::first_unresolved`, over open *or* abandoned entries, and
  `outstanding` counts that same set: the abandonment is named first, because it
  is the entry that explains every successor blocked behind it.
- **A settlement now names the generation it settles, and one naming any other
  is refused.** A chain holds one transition at a time and reuses its slot for
  every generation over it, so `(chain, entry)` alone could not distinguish a
  report about the attempt the caller was shown from one about the attempt that
  replaced it. `Bot::resolve_effect` read a delayed report for revision N against
  revision N+1: `Applied` acknowledged an effect at a generation nobody had
  asked about, and `NotApplied` — the sharper half — made an attempt the caller
  was never shown eligible to run again, which is a duplicate merge, message or
  launch. `resolve_effect` now takes the `revision` that `Bot::pending()` reports
  with the work, compares it before reading or writing anything, and refuses a
  superseded generation with `BotError::EvidenceSuperseded`, carrying both the
  named and the live revision so the caller can re-read and report again. This
  is a breaking signature change; the revision is not inferable, which is why it
  is required rather than defaulted.
- `BotError::EvidenceContradicted` is the second new refusal: evidence saying the
  opposite of what already settled *this* generation's entry. Repeating the same
  evidence is not that — it succeeds idempotently, so a caller whose first
  delivery was ambiguous can send it again without having to know whether it
  landed. Neither new variant is `NoSuchWork`, which keeps meaning exactly "there
  is no held effect at this address"; collapsing the three would leave a caller
  reading a stale-report refusal as a wrong address, which is a different repair.
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
  lines inside their doc blocks rather than at the functions — a class
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
