# lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04 (11/13)

Release: lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04. Page 11 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Added (continued)

- Host-held resumable runs (#87 step 5). `HostBuilder::run_store(dir)` installs a
  durable, file-backed per-step record store — `task::RunStore`, chain-framed and
  `fsync`-ed per record, opened and replayed at installation so a refusal happens
  before a run claims durability. `script::remember(&scope, "step", || async
  { .. })` returns a committed step's decoded value **without polling its
  future**, and otherwise runs the future, syncs the record, and returns it.
  `Host::run` mints a `RunId` when a store is installed; `Report::run_id()` names
  it; `Host::resume(run_id, &task, input)` re-runs the body with recorded steps
  replayed, and `Host::resume_ticket` takes a `Report::ticket()` naming the run,
  its owning tenant, its disposition and the step path it stopped at. Records are
  keyed by tenant, so resuming another tenant's run id is a typed `Refused`; a
  broken chain is refused rather than trimmed; an interrupted final append is the
  only thing dropped; and the three ceilings (per-record bytes, records per run,
  total bytes) are typed refusals that leave the store byte-identical.
  `Report::effects()` gains `EffectKnowledge::StepRecords { records }` beside
  `None`; without a store the report says `None`, names no run, and offers no
  ticket, so no in-memory sink is ever reported as durable. Documented honestly:
  a step that ran but whose record did not land re-runs on resume — exactly-once
  for a recorded step, at-least-once for an unrecorded one — and an external
  effect still needs the effect journal. Migration: none; every added item is
  additive, and a host that installs no store behaves exactly as before.
  INV-BOT-54.
- `domain::gh` is a real adapter (#151): the GitHub CLI runs as one supervised
  child through `Supervisor::run_process`, with bounded capture, a deadline and
  process-group cleanup, instead of the typed `binding required` refusal the
  domain returned before. `Gh::snapshot`, `Gh::read_reviews` and `Gh::publish`
  each admit a validated `ProcessSpec`; arguments are a vector, so no shell is
  involved. `Repository`, `CommitId` and `ReviewPayload::new` refuse what
  GitHub would reject rather than sending it. A publication payload is staged as
  a private file (`create_new`, mode 0600) and removed on every exit path; the
  name comes from `lgwks_std::random` under `ephemeral`, and a build without
  that feature refuses to publish rather than reuse a name the OS recycles.
  Without the `process` feature every call refuses with `GhError::NoRunner`
  rather than reporting an empty answer a caller could mistake for "GitHub has
  no reviews". `GhQuery` and `PrSnapshotSource` expose the same binding through
  `Query` and `Observe`; both require `bot.sys` and `bot.net`.
- `review`, the canonical PR-review task (#87 step 6, #151): `review_pr` pins a
  subject, runs a caller-supplied analysis, checks freshness, publishes once at
  the reviewed commit, and verifies through a separate read-back.
  `ReviewOutcome` is five states a caller can act on without reading a message —
  `Published { verified }`, `Unknown`, `TargetMoved { reviewed, current }`,
  `Refused`. A lost response is reconciled by one read, never a second create;
  only a `Refused` certainty proves nothing was written, because a non-zero
  exit cannot distinguish "never arrived" from "applied and the answer was
  lost". Verification compares subject, body and state and deliberately ignores
  the application marker, which locates a candidate and is not proof. The body
  manages no pid, no reap loop and no retry over publication.
  `cargo run -p lgwks_bot --features process --example review_pr -- <repo> <pr>
  <EVENT> <body>` runs the whole path; `LGWKS_REVIEW_PUBLISH=0` is a
  draft-only profile.
- `domain::gh::MAX_REVIEWS_PER_PULL` and `GhError::ReviewCeiling` (#151):
  `--paginate` follows GitHub's review pages until the client is done, so the
  review list used to grow with a pull request's history rather than with any
  bound of this crate's own. A review read now refuses a list longer than the
  declared ceiling — a *typed refusal*, not a shortened list. The distinction
  matters because a truncated list that decoded cleanly is indistinguishable
  from the whole history, and a verification built on it would report "no
  matching review" for a review that exists on a page nobody read. Exactly the
  ceiling is accepted; one more is refused, naming the endpoint, the count and
  the ceiling. This is a bound, not a truncation: a build without the
  `process` feature still refuses every call with `NoRunner` rather than
  reporting an empty snapshot, an empty review list, or review id `0`.
- `inspect`, typed in-process structural code inspection (#150, R8; feature
  `inspect`): `inspect(&InspectRequest)` parses the subject's bytes with
  `lgwks_ast` and walks the tree against the versioned `RuleSet::STRUCTURAL_V1`
  rules (`rust/no-unwrap`, `rust/no-todo`, `rust/no-panic`), returning an
  `Inspection` whose typed `Verdict` distinguishes `Clean` (no match within an
  explicitly complete supported scope), `Violations`, `Unsupported` (no compiled
  grammar, unknown/mismatched rule set, or a grammar the rules cannot read),
  `Undecidable` (a declared language version this build cannot confirm),
  `Incomplete` (source/node/depth/work/findings/output budget exhaustion or parse
  recovery) and `InfrastructureFailure`. The subject is never compiled, imported,
  built, shelled or loaded; `Budgets` bounds bytes, nodes, depth, work, findings
  and output as separate axes, and `Inspection::coverage` reports per-rule
  support and evaluation. The report serializes through the shared JSON facade
  with its input digest, exact byte spans and rule revision intact, and
  `Inspection::assurance` states that a clean result is not proof of safety.
  Host-only, like `process`/`fs`/`net`: it draws native tree-sitter grammars, so
  it is not in the default feature set; `full` enables it.

- `domain::inspect`, the same operation wired onto the verbs and the task front
  door (#150, R8): `Inspector` is a `Query` over an `InspectionJob` (caller-
  supplied bytes, no capability), and `Subject` is an `Observe` source that
  reads the artifact under `bot.fs` (bounded before the read), so a `BotSpec` or
  a native bot reaches `inspect` through the registry and admission path every
  other domain uses, and `inspection_task` exposes it as a `Host`-run `Task`.
  Every door returns the operation's own `Inspection`. The `inspect_scale`
  example is the print-only measurement harness (per-tier latency percentiles
  and per-size `Resources` counters) for `/usr/bin/time -l`.
