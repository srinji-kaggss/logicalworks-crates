# lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04 (12/13)

Release: lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04. Page 12 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Added (continued)

- `task::{Host, Task, Report}`, the front door (#87 step 1): build a `Host` once
  (tenant, stop token, admission ceiling, default deadline, trail capacity, all
  finite and readable through `Host::limits`), define a `Task` with `task(name,
  body)`, and `host.run(&task, input).await` returns a `Report` carrying the
  disposition (`Succeeded`, `Failed`, `Cancelled`, `DeadlineExceeded`,
  `Refused`), the typed output, the located error, a bounded step trail with a
  dropped-step count, and `EffectKnowledge::None`. Bodies are polled on the
  calling task, so they need not be `Send` and inputs may borrow; nested runs
  on one host are charged to the parent's permit (INV-BOT-20).
  `Host::block_on` is the synchronous entry and refuses to run inside an
  existing runtime.
- `Bot::from_spec` materializes a validated `BotSpec` against a `DomainRegistry`
  into a runnable `Bot`, through the same `assemble`/`build` path a native bot
  uses — no second interpreter and no second execution path — so a JSON-built bot
  and a natively built one produce the same operation trace. Authority still
  comes only from the caller's `GrantSet`, never from the document. Admission is
  all-or-nothing and reports every presently knowable unmet need in one
  attributed value (`Admission`, `NeedSet`, `Need`): an unknown source or action
  domain, a constructor that rejects its target, a missing capability, or an
  unknown condition. A duplicate registry identifier and an unsupported spec
  version are typed refusals, and nothing is polled or executed on refusal. (#87)
- `Condition`: an erased condition handle, so a materialized chain and a native
  one share one condition erasure and one type-mismatch report.
- `Source::condition` resolves a wire condition identifier against the source's
  own output type, from the closed vocabulary `changed`, `always`,
  `threshold::above(<n>)`, `threshold::below(<n>)`.
- `Supervisor::run_process` runs one child under the same in-flight ceiling,
  process-group ownership and deadline machinery as `spawn_process`, and returns
  its `ProcessRun`: the exit status or signal, bounded captured stdout and
  stderr with truncation accounting, whether a deadline stopped it, and the
  process-group cleanup receipt.
- `rt::process::StdioPolicy::Capture(NonZeroUsize)` captures a standard stream
  up to a non-zero ceiling, keeps draining past it, and reports the exact total.
- `domain::sys::Process` runs real processes with the `process` feature and
  reports exit code or signal, the captured streams and a finite default
  capture ceiling and deadline; without the feature it keeps its refusal.
  Concurrent verb calls on one `Process` share a bounded slot pool
  (`DEFAULT_MAX_CONCURRENT`, set with `Process::max_concurrent`), claimed
  before the fork, so a burst of calls never forks a burst of children.
- `FileJournal::replay` returns a `Replay` that streams the committed frames
  from its own read-only descriptor, one event at a time, so a caller that only
  folds the record pays for the largest frame rather than the whole history.
  It applies the same frame validation `open` does and is bounded by
  `MAX_JOURNAL_EVENTS`; the materialized `events()` view is unchanged (#122
  item 2).
- `StorageGate::storage_gate` is public, so a slow-store liveness test can
  release a parked device from its own thread rather than from the runtime
  awaiting the append (#156).
- `EffectJournal::reserve_handoff_capacity` reserves room for a whole external
  handoff — intent, preparation and the settlement that lands after the effect
  has left the process — before the first rung is written. The default is
  permissive; `FileJournal` refuses the handoff against its event ceiling, so a
  durable journal can never leave an attempt admitted and unable to settle
  (#122 item 2 / #156).
- The `compare_orchestration` example gains a `host` way — `Host::run` per item
  with the host's admission ceiling as the fan-out bound and matched semantics
  against the hand-written `JoinSet`+`Semaphore` and `join_all_bounded` ways —
  and a `measure_overhead` example prints p50/p95/p99 for `Host::run` and
  `sys::Process`.

### lgwks_deps Added

- `[[approved]]` entries accept an optional `origin`: the exact admitted origin
  for the entry's source class — a complete registry source, a Git repository
  plus its admitted revision/reference policy, or an external path authority.
  Admission now compares origin as well as class, so replacing an approved Git
  repository, registry, path, or Git revision produces a typed
  `Refusal::OriginDrift` carrying the approved and observed identities. A legacy
  class-only entry is exact for crates.io (both its Git and sparse spellings)
  and insufficient for a Git or path edge; an unknown origin scheme is refused
  at load and never admitted (INV-DEP-12, #158 A1).
- Sparse-registry sources (`sparse+…`) are classified as the `registry` source
  class rather than an unknown scheme, so a sparse crates.io mirror compares as
  crates.io.
- Drift diagnosis with several approvals for one crate reports the dimension on
  the approval that admits the edge's source class, instead of the first
  mismatch from an unrelated class (#158 acceptance).
- `[[approved]]` entries accept the admitted-capability policy keys `features`,
  `required_features`, `uses_default_features`, `optional` and `target`, and an
  explicit `aliases` list. `DirectEdge` now carries Cargo's authored `features`,
  `uses_default_features`, `target` and `rename`, so a capability that changes
  without a class or origin change is a typed `Refusal::FeatureDrift`,
  `DefaultFeaturesDrift`, `OptionalityDrift` or `TargetDrift` instead of a pass.
  A dimension an entry does not author is grandfathered (#158 A2, INV-DEP-13).
- `metadata::DirectEdge::features`/`uses_default_features`/`target`/`rename` are
  readable through accessors; `rename` is the manifest-local spelling and
  `package` remains the upstream Cargo identity (#158 A2).
- `check` prints a receipt binding the subject root, the contract identity and
  schema version, the exact metadata subject, the policy mode and the assurance
  scope, and `check --json` exposes the same under the stable keys `mode`,
  `contract.{digest,schema,entries,repository}`, `subject.{digest,edges,resolved}`
  and `scope`. `Contract::digest`/`schema` and the `Subject`/`Verdict` types back
  it; `check_verdict` returns the receipt-bearing verdict (#158 A6, INV-DEP-15).
- A register may author `[policy] schema`; the committed register is migrated to
  `schema = 2`. Schema 1 remains readable (#158 A7).

### lgwks_deps Changed

- **Breaking for a Git or path edge: a class-only approval is insufficient.**
  An `[[approved]]` entry whose `source` is `git` or `path` and that authors no
  `origin` admits nothing in that class; it is no longer an implicit approval of
  every origin. **Migration:** add `origin = "<exact source>"` to each Git or
  path entry (a Git repository plus its `?rev=`/`?branch=` policy, or the exact
  path authority). A class-only `registry` entry still admits crates.io in both
  its Git and sparse spellings, so registry entries need no change (#158 A1,
  INV-DEP-12).
- Package and owner matching is now byte-exact against the Cargo-authored
  identity. **Migration:** a register that relied on the implicit `-`/`_` (or
  case) fold to match a differently-spelled package must either write the exact
  Cargo name or add `aliases = "<spelling>"` to that entry; an alias is
  collision-checked and names exactly one package. The committed register uses
  exact names throughout and needs no alias (#158 A2, INV-DEP-14).

### lgwks_bot Changed
