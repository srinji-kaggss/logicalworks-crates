# lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04 (7/13)

Release: lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04. Page 7 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Added (continued)

- Group commit on `journal::owner`: concurrent durable appends now share one
  `fsync` instead of paying one each (#152). An ordered step became two phases —
  every queued request's checks, fence, framing and `write_all` run in submission
  order, then **one** `sync_all` covers the whole batch, and only then are the
  answers published and the records folded. No append is acknowledged before the
  flush covering its bytes returns `Ok`; a failed batch acknowledges *none* of its
  members and latches the poison once for all of them, exactly as a failed single
  append did; a dropped waiter still poisons the handle; and the layout order,
  the length fence and the hash chain hold across batch boundaries.
  Measured, release build, same harness and payload as the merge base built in a
  separate target directory: at concurrency 16, 256 appends went from **2.913s
  (88/s, 256 fsyncs)** to **0.184s (1,394/s, 25 fsyncs)** — 15.9x, at 0.09
  fsync per record, with p50/p95/p99 falling from 149,839/396,700/590,351us to
  7,987/47,991/48,101us. A lone append still pays exactly one `sync_all` with no
  linger (16 fsyncs for 16 sequential records), because the owner drains what is
  queued when it wakes rather than waiting for a batch to fill. Bounded and
  declared: `MAX_BATCH_RECORDS` and `MAX_BATCH_BYTES` cap one flush, and a
  caller arriving at a full request ring now *waits* in a second bounded ring
  rather than being refused — which is why a store that previously refused outright
  with `QueueFull` at concurrency 256 now serves 128 concurrent submitters and
  refuses the 129th.
- `RunStore::flush_counts`, the mechanism's own `sync_all` and staged-record
  counters (#152). The batching factor is the one claim in this change that a
  latency difference could only hint at, so it is measured where it happens — on
  the owner thread — and read through the store's public surface. A caller that
  gave up on its answer is still counted as a staged record, so the ratio cannot
  report a better batching factor than the store achieved.
- Two missing arms of the request-keyed submission are now covered rather than
  assumed. `DeadlineExceeded` is the request's own verdict — the same declaration
  that fixed the key also fixed the run's budget — so a request that overran its
  budget reattaches to that recorded deadline instead of re-running a body that
  has already overrun once, and the durable step recorded before the overrun
  survives it. A store that refuses the `@terminal` write during `Host::resume`
  is reported as `Failed` naming both that the outcome went unrecorded and which
  bound refused it, and records **nothing**, so the request stays unsettled and a
  later client reads `InFlight` rather than a verdict that was never written.
  Enforced by `tests/request_key.rs`
  (`an_expired_deadline_is_the_requests_recorded_outcome`,
  `a_store_that_refuses_the_terminal_write_reports_the_refusal`) and by the
  seeded families `expired_deadlines_are_recorded_band_16..23` and
  `settle_refusals_are_reported_and_leave_the_request_unsettled_seed_a..p` in
  `tests/sim_request_key.rs`, two tenants over one store file, each seed swept
  twice for an identical trace hash. No production behaviour changed.
- `Disposition::Blocked` is not the request's own verdict, so a blocked run
  records no `@terminal` — recording it would poison its key exactly as a host
  stop does, and an authorized repair can still move it. It carries a
  `DISPOSITION_BLOCKED` code so `disposition_code` stays an exhaustive match over
  `Disposition`, which is what INV-BOT-102 relies on to catch a later variant.
- `Host::submit`: a request-keyed durable submission. A caller supplies a
  `RequestKey`; the host derives the run identity from the tenant and the key,
  records the input's canonical `InputDigest` under a reserved `@request` record
  before any step runs, and records the terminal outcome under `@terminal`
  after. `Submission::Executed` is a first run, `Submission::Reattached` returns
  the recorded report without re-entering the body, `Submission::InFlight`
  reports a request whose waiter was dropped mid-effect (the run to settle and
  the records that survived, kept apart), and a different input under one key is
  a typed `RequestError::Conflict` naming both digests. A host with no store
  refuses with `RequestError::NoStore`. Supported by `RequestKey`, `InputDigest`,
  `Submission`, `InFlight`, `RequestConflict` and `RequestError` (T30, T17;
  INV-BOT-100, INV-BOT-101).
- `FlowError::InvalidRequestKey`: the typed refusal a malformed `RequestKey`
  names, alongside the tenant and task-name refusals.
- Fourteen seeded simulation families, one per refusal arm of the review
  subject, coverage and partial-submission path (#231, INV-BOT-96). No
  production behaviour changed: `subject_coverage_and_partial_faults` asserted
  that every fault reaches *some* correct outcome variant, which is satisfied by
  a world in which the right arm is reached for the wrong reason, so each arm
  now asks a different question of the same seeded worlds — a coverage refusal
  reaches the receiver with zero creates; the file-count and byte ceilings are
  refused on separate axes, each naming its own bound, with the byte family's
  draws asserted to stay under the file ceiling so a refusal for the wrong bound
  cannot pass as evidence for it; a renamed repository is refused naming both the
  requested and canonical repository; a `build.rs` in the changed-file inventory
  is never executed, proven by a marker file named in the child's own environment
  and referenced by the patch text; a lost response onto a draft is `Pending`
  and a partial submission reports both the applied and intended counts, neither
  issuing a second create; a create whose read-back lost permission is
  `Unverified` with the applied review id retained and inside the receiver's own
  id range; the permission and transport arms are disjoint, naming an HTTP status
  and a credential versus a child's exit and no status, and are `Unverified` and
  `Unknown` through the journey; a publication is pinned to the commit that was
  read; two tenants' coverage verdicts stay isolated; and concurrent runs on one
  pull request conserve their creates and their read-backs. The test file's share
  of the repository's deterministic-simulation evidence is restored, which the
  `simulation-evidence` gate requires at one half of all tests.
- `domain::gh::read_diff`, a bounded changed-file inventory for the PR-review
  subject (#87 step 6, T31): `Gh::read_diff` reads a pull request's changed
  files as **data** and bounds them on two separate axes — at most
  `MAX_DIFF_FILES_PER_PULL` files (`GhError::DiffFileCeiling`) and at most
  `MAX_DIFF_BYTES` bytes of patch text (`GhError::DiffTooLarge`) — refusing the
  whole inventory rather than truncating it. A server that declines to render
  the diff (`406`) is `GhError::DiffUnavailable`. A renamed repository is
  `GhError::MovedRepository` naming both the requested and canonical
  repositories, so the subject identity is never silently re-pointed. A client
  that names an HTTP `401`/`403`/`404` is `GhError::Unauthorized` rather than a
  transport failure. `GhOutcome::http_status` exposes the status the client
  named. No file in the inventory — including a `build.rs` — is compiled,
  imported, built, shelled or loaded (INV-BOT-21).
- `ReviewOutcome::{Incomplete, Pending, Partial, Unverified}` (#87 step 6,
  T31/T33/T34): an unavailable or over-ceiling diff is an `Incomplete` coverage
  decision (nothing is published); a lost response reconciled onto an
  unsubmitted draft is `Pending`; a submitted review that landed with fewer
  inline comments than intended is `Partial` with both counts; and a create
  that returned an id whose read-back lost permission is `Unverified` with the
  applied review id retained. No path issues a second create, and a read that
  failed for any other reason stays `Unknown`. `ReviewComment`,
  `ReviewPayload::with_comments` and `ReviewRecord`'s comment-count comparison
  carry the inline comments the partial check reads. INV-BOT-81.
