# lgwks_bot: failure

Pages of this README: [overview](README.md) · [execution model](README-execution.md) · [failure](README-failure.md) · [authority, domains and specs](README-authority.md) · [script!](README-script.md) · [async runtime](README-runtime.md) · [supervision](README-supervision.md)

### Failure

`tick` and `tick_async` return `Err(BotError)` in three situations, and they
mean different things. A fourth `Err` is not a failure of the tick at all — the
synchronous adapter refusing a call site — and it is documented at the end of
this section rather than among them.

**A poll failed.** No `Revision` was written, so no condition was evaluated and
no action ran. The tick had no effect, and the source values are exactly as they
were. This is the strong case, and it is the one an all-or-nothing reading is
tempted to generalise from — it does not generalise.

**An action failed.** Actions run in declaration order, so the actions *before*
the failure already ran and their effects are live. `tick` reports the first
error and does not roll anything back, because an external effect cannot be
rolled back. `Err` here means "this run did not finish", not "nothing happened".

**A condition failed.** A condition that errors rather than returning `false`
stops the walk at its position: the effects before it are live and the effects
after it are not attempted, which is the same shape as a failed action. The
first failure in walk order is the one reported, so an action failure *before* a
condition failure is the error you see.

**The synchronous adapter refused.** `tick` returns
`BotError::TickInsideRuntime` when the calling thread is already being driven by
an async runtime. Nothing happened: the check is the first thing the adapter
does, and the same bot awaits correctly on that runtime. It is not a failure of
the tick, it is a failure of the call site. See
[Running a tick from async code](README-execution.md#running-a-tick-from-async-code).

Two consequences follow, and neither is fixed by retrying blindly:

- **The unattempted work is queued, not lost.** Work that is eligible stays
  recorded: an entry that has not been attempted, or one whose attempt failed
  under a budget that is not yet spent, is attempted on a later tick even if the
  source never moves again, and the entries after it are not skipped to reach
  anything. `tick` returns `Err(BotError::PendingTransition)` while work is held,
  so a clean tick is never "the transition was handled" when it was not, and
  `Bot::pending()` lists every entry that is not finished — including any entry
  the attempt budget gave up on, with its reason. An entry that was given up on
  is a *barrier*, not a step behind you: the entries after it are not attempted
  while it stands, because declaration order is a prerequisite chain, and they
  stay reported behind it. Evidence
  (`EffectEvidence::NotApplied`) is the way past. `RetryPolicy` sets the budget
  (three attempts by default, `RetryPolicy::ONE_ATTEMPT` for none).
- **A retry may duplicate.** An action that failed after its request was sent
  fails as `BotError::EffectIndeterminate`, which says the effect may be live.
  That variant exists precisely so this is readable from the type rather than
  inferred from a cause string: `BotError::DomainError` means the effect did not
  happen and a retry is a retry, while `EffectIndeterminate` means a retry is a
  possible duplicate — a second merge, message, or process launch. Consumers
  that retry should match on the variant and treat these two differently.

An effect that may already have happened is never re-attempted on its own: the
entry is held and reported, and `Bot::resolve_effect(work, revision, evidence)`
is how a caller says what happened — `EffectEvidence::Applied` records it without
replaying it, `NotApplied` makes the entry eligible for an attempt again. The
`revision` is the one `Bot::pending()` reported with the work, and it is not
optional: a chain reuses one slot for every generation over it, so the revision
is the only part of the identity that says *which* attempt the evidence is about.
A report naming a generation that has since been superseded is refused with
`BotError::EvidenceSuperseded`, and one contradicting the evidence already
recorded for its generation is refused with `BotError::EvidenceContradicted`;
repeating the same evidence succeeds idempotently, so a delivery whose outcome
the caller never saw can be sent again.

Delivering exactly-once across an external effect still needs durable intent
outside this process: the ledger is in memory, so it reports an unsettled effect
to the caller that owns it rather than surviving a crash. A panic that unwinds
out of an action takes the chain's live transition with it; see the limits
section in `src/ecs.rs`.
