# lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04 (4/13)

Release: lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04. Page 4 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Fixed (continued)

- A host-side stop is no longer recorded as a request's outcome. `Host::submit`
  wrote a `@terminal` record for every disposition that was not a success, so
  `Cancelled` (the host's stop arrived mid-run) and `Refused` (the host declined
  before admission) became the request's permanent recorded verdict. Since the
  key *is* the request's identity and its body runs at most once under it, one
  shutdown left a key that no later submission could ever complete: every repeat
  reattached to the stop and the body's recorded durable steps were never
  resumed. `@terminal` is now written for exactly the three dispositions that
  are the request's own verdict under its declared task — `Succeeded`, `Failed`
  and `DeadlineExceeded`, the deadline included because the same declaration
  that fixed the key also fixed the run's budget — and a host stop writes
  nothing. The classification is one exhaustive `match` over `Disposition`, so
  a variant added later fails to compile until its relationship to a request
  key is decided by hand. The stop is still reported to the caller that saw it
  (`Submission::Executed` carrying the disposition); what no longer happens is a
  restart turning into a request that can never succeed. INV-BOT-102.
- `Host::resume` settles a request that `Host::submit` left incomplete, under
  the same rule: a resumed run whose receipt exists and whose verdict does not
  records that verdict, so a request interrupted by a host stop (or by a client
  that walked away) can reach a recorded terminal at all instead of reporting
  `InFlight` forever. Settling is deliberately narrow — `Host::run` writes no
  reserved record, a resume of a run with no `@request` receipt is an ordinary
  resume, an already-settled run is left alone, and a `Refused`/`Cancelled`
  report is returned untouched, so a cross-tenant resume stays `Refused`. A
  store that refuses to record a verdict a run *reached* is reported as
  `Failed`, since recording an outcome and reporting success are one fact.
- **Breaking:** `Host::resume` and `Host::resume_ticket` now require
  `O: lgwks_bot::script::Durable`. A resumed run may be settling a request, and
  a recorded verdict is made of an archived output. `Host::run` is unchanged.
  **Migration:** a caller whose task returns a value that is not archivable must
  return a `Durable` one, or route the run through `Host::run` plus an explicit
  run id it owns rather than a resume.
- `Host::repair` settles a request too. A request `Host::submit` started that
  blocked on authority records no verdict (`Blocked` is not the request's
  outcome), and the repair is the attempt that reaches one — but it did not
  record it, so a key repaired to success still answered `InFlight` to every
  later submission until a separate `Host::resume` ran. The repair now settles
  under the same rule as `resume`, and the key reattaches, from a reopened store
  as well (`tests/request_key.rs::a_repaired_request_is_settled_and_reattaches`).
- **Breaking:** `Host::repair` now requires `O: lgwks_bot::script::Durable`, for
  the reason `Host::resume` does. **Migration:** as for `Host::resume`.

### lgwks_bot Changed

- `rt::supervise::CleanupReceipt`'s documentation now states what
  `CleanupConfirmed` does and does not claim. It claims that every process still
  *in the supervised group* when the group was last observed is gone — an
  observation of `killpg(group, 0)`. It does not claim that no process the
  supervisor started is still running: a descendant that called `setsid` has
  left the group by construction, so its survival is not a counterexample.
  Nothing about the type or its variants changed; a caller needing the stronger
  guarantee needs a kernel job object or a cgroup, which this crate does not
  have. Accepting row T21.

### lgwks_std Breaking

- `similarity`: the `Similarity` implementation for `Cosine` now returns the
  normalized `(raw + 1) / 2` score instead of raw cosine, so the trait's
  documented `[0.0, 1.0]` interval holds for every implementation behind it
  (#160 S1). **Migration:** a caller reading `Similarity::score` for a cosine
  comparison and comparing against a raw-cosine threshold must call
  `Cosine::try_score`, whose `[-1.0, 1.0]` domain is unchanged. The named
  mapping is `Cosine::normalized_score`.
- `similarity`: `ComponentOutcome` and `EvidenceVerdict` expose accessors
  (`index`, `weight`, `score`, `reason`, `outcomes`) instead of public fields
  (#160 S2). **Migration:** replace `verdict.score` with `verdict.score()` and
  `outcome.index` with `outcome.index()`. The fields are private because an
  outcome is a report about a measurement; a caller that could set a refused
  component's score would defeat the contract.

### Fixed
