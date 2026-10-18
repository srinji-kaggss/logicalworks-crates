# 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above (9/13)

Release: 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above. Page 9 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Fixed (continued)

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
