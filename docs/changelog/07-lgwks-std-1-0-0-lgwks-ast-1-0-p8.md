# lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04 (8/13)

Release: lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04. Page 8 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Added (continued)

- Repairing a blocked run, end to end (feature `script`; `ephemeral` for minting
  a run id). A step reaches for authority with `Scope::require(&[Cap])`, and a
  run whose authority — the host's grant plus any repair delta — does not cover
  it is `Blocked` with a complete `Deficit` rather than `Failed`: the host was
  willing and the authority was missing, which is the distinction a repair acts
  on. `Report::needs` and `Report::repair` are both derived from that one value,
  so they cannot disagree. `Host::repair(ticket, grant, task, input, spend)`
  resumes under the run's own id, so the steps recorded before the block replay
  without their bodies being polled and only the blocked remainder runs. The
  grant may not be short (`RepairError::NotAuthorized`) nor carry any capability,
  shipped or custom, the ticket does not name (`OverWide`); the authority the
  repaired run receives is built from the ticket's needs, never taken from the
  grant, and the host's own grant is never widened — the next run on that host is
  still blocked. `HostBuilder::grants`,
  `repair_ledger` and `repair_bounds` configure it.
- `task::RunLedger`: the durable per-run control ledger — root spend and attempt
  budget, repair epoch, and the set of applied tickets — over the shared frame
  grammar and the shared storage-owner thread (INV-BOT-51). A ticket's identity is
  its content (run, tenant, epoch, sorted needs), so the same ticket delivered
  twice is refused `AlreadyApplied` and an older ticket is refused `StaleEpoch`;
  decide-and-write is one ordered step on the ledger's own thread, so "applied
  once" is a fact about bytes. A refused repair charges nothing, mints no epoch
  and leaves the ledger byte-identical. A repair consumes the root budget rather
  than refilling it, so a permanent refusal plus repeated `NotApplied` reaches a
  finite `BudgetSpent` (#87 step 3, T13/T23/T24).
- `task::repair`: `RepairTicket` (a report, never a grant), `RepairError` with a
  typed arm per refusal.
- Deterministic-simulation evidence for the repair door, one seeded family per
  arm rather than one family that reads a final counter (#87 T13/T23/T24). The new
  families in `tests/sim_repair.rs` are: `the_step_that_reaches_is_the_step_that_blocks`
  (a `Task::requiring` reach is `Blocked` at the admission boundary with no body
  poll, no permit, no record and no root attempt charged) and
  `a_wide_need_set_costs_one_analysis` (a shortfall of one to four capabilities
  costs exactly one analysis, and the report's and the ticket's needs are the same
  set in the same order);
  `a_custom_capability_is_refused_at_every_width` (the over-wide check walks the
  grant rather than a candidate list, so a custom name is refused at one need and
  at four, and the ledger is left byte-identical) and
  `a_ticket_never_names_another_tenants_run` (the ticket's own tenant check
  refuses before admission and before the ledger, so the asking tenant's ledger
  never gains an entry);
  `a_mixed_decision_order_pins_each_arm` (each arm observed per decision rather
  than inferred from the endpoint), `every_repair_charges_the_root_budget_once`
  and `a_spent_budget_refuses_every_later_attempt` (the budget sequence, and a
  finite refusal at either ceiling that charges nothing and is stable across later
  attempts), `a_reopen_reads_back_the_charged_budget` and
  `a_repaired_run_survives_a_reopened_host` (the replay rests on bytes a second
  host opened), `a_host_spent_on_one_run_still_repairs_the_next` (a spent run's
  ceiling bounds that run alone, and recovery after the refusal is through a
  reopen — the handle the refusal poisons is INV-BOT-50's, not this door's), and
  `a_bounded_sweep_repairs_every_ticket_once` (100 and 1,000 runs over one
  ledger, each ticket applied exactly once and each run charged exactly twice; the
  10,000 tier of the declared claim stays on the opt-in
  `the_declared_repair_tiers_are_measured`, measured at 488 s against 48 s for the
  two that run on every ordinary pass).
  No behaviour changed; these are the properties INV-BOT-33/34/35 stated with no
  arm-level evidence, recorded as INV-BOT-36/37/38.
  **Known limit, stated rather than left to be discovered:** `Host::repair` still
  has no production caller inside `lgwks_bot` — it is the door the front door
  exposes and an embedding host calls, so every exercise of it here is a test. The
  behaviours it gates (the blocked disposition, the ledger charge, the ticket) are
  all on the real `Host::run` path, and this package adds no new capability that
  only tests reach; it widens the evidence over the one that had a single point.
- `task::Control` is re-exported from `lgwks_bot::task`. `RunLedger::control`
  already returned it — a caller reading a run's budget or epoch had to name the
  type to hold it, and there was no path to the name — so the name is now public
  beside the handle that returns it. Additive: no existing signature changes.
- `Disposition::Blocked` on the front door, distinct from `Refused`: a `Refused`
  run was refused by the host and no authority would change it, while a `Blocked`
  run is the one an authorized repair can move. `Task::requiring` is the blunt
  form for a task that reaches in its first step; `Scope::require` is the one that
  leaves the work before the block replayable. `FlowError::Blocked` is
  `#[non_exhaustive]`-added and never retryable.
- `proposal` (feature `script`): the boundary where untrusted model output
  becomes work. Issue #87's rule is that an AI proposal is an **untrusted task
  input**, and that validation, provenance, no-progress detection, bounded repair,
  tenant-scoped artifacts and serialized writes belong in the host/adapter
  contract rather than in a prompt — so this module reads bytes and refuses them
  rather than prompting a model. It is **not** a fifth verb: an admitted `Plan` is
  a list of names to perform through the existing verbs, the operations a proposal
  may name are exactly the ones the host registered in a `Surface`, and the crate
  calls no network model at all. `StubModel` is a deterministic double from a seed
  to output bytes, because the guarantee here is about *admission* and admission is
  identical whoever produced the bytes (#87 T26–T29, T35).
  - `Decoder`: a hand-written bounded line grammar with a byte ceiling, a
    per-field ceiling and a field-count ceiling. `install`, `credential` and `host`
    are *recognised* so their refusals name what was asked for — a decoder that had
    never heard of them would report `Malformed` and an attempt to widen authority
    would read as a broken document. An unknown field is refused rather than
    ignored, and a refused payload returns no plan beside its refusal.
  - `Refusal`: a typed arm per refusal, with `SandboxEscape` as its own arm so an
    escape stays observable, and `is_privilege_attempt` for the five arms that are
    attempts rather than syntax. Every outcome carries `Provenance` — the source
    (model, tool output or document), the tenant and the digest of the exact bytes.
  - `Completion`: admitted only with the evidence it names present.
    `Coverage::from_claim` maps *every* payload claim onto `Partial`, so a plan
    cannot be talked into full coverage, and `Coverage::Complete` has no
    constructor reachable from a decoder.
  - `RepairLedger` and `PlanBudget`: one unchanged fingerprint past its ceiling is
    a typed `Intervention`, and recording new evidence moves no repetition count,
    so it cannot erase what a failure already cost.
  - `Checkpoint`: `Durable`, so a context reset recovers completed steps, user
    corrections *with their kind*, `Unknown`-classed effects and evidence
    references through the run store.
  - `ArtifactStore`: keyed by `(tenant, digest)`, so identical bytes from two
    tenants are two artifacts; writes to one key serialized and idempotent by
    content, reads lock-free of the writer, every bound a typed refusal that
    leaves the store unchanged.
