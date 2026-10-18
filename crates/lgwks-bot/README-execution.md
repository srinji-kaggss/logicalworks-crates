# lgwks_bot: the execution model

Pages of this README: [overview](README.md) · [execution model](README-execution.md) · [failure](README-failure.md) · [authority, domains and specs](README-authority.md) · [script!](README-script.md) · [async runtime](README-runtime.md) · [supervision](README-supervision.md)

## Execution model

A `Bot` owns a `bevy_ecs::World` and a `Schedule` with exactly two systems,
`observe_fold` then `fire_plan`. Both are **exclusive systems** (`fn(&mut World)`)
and neither awaits: they commit observations and record a decision. One tick is
four phases, of which only the two middle ones are those systems:

1. **Observe** — poll every source in bounded concurrent waves (32 in flight)
   and await each wave. This happens on the caller's executor.
2. **Fold** (`observe_fold`) — compare each result with the remembered one and
   bump that source's `Revision(u64)` marker component *only when the value
   moved*. A poll that failed is reported here, before any `Revision` is
   written. The remembered value is the one the chain last acted on, so while a
   transition owns it the comparison is against that transition's binding.
3. **Decide** (`fire_plan`) — walk the bot's *eligible work*, recorded in a
   ledger keyed by `(chain, entry)`, in declaration order: a chain whose
   `Revision` moved opens or resumes a transition, and a chain with work
   outstanding is walked whether or not it moved. Evaluate each entry's
   condition and record the effects whose conditions hold, in declaration order.
   Conditions are pure over the observed value, so this records exactly the
   program the old interleaved loop would have walked.
4. **Act** — apply the recorded decisions in that order, awaited one at a time,
   so side effects stay deterministic, writing each entry's outcome back into
   the ledger as it goes. A chain's walk stops at the first entry it cannot
   settle, so an acknowledged effect is never replayed to reach a successor.

The ledger in phase 3 is what makes `Changed<Revision>` a *trigger* rather than
the whole answer: the change filter decides which chain opens a transition, and
the ledger decides what that chain still owes from then on.

**A transition is bound to one observed payload.** Opening or resuming a
transition *moves* the observed value out of the fold's slot and into the
transition, and every entry of that transition — every condition evaluated and
every action run — reads that binding, never the newest observation. A value
that arrives while a transition still has entries open is not admitted; it is
still in the slot, and it becomes the next transition's payload on the first
tick after the current one has nothing open. Without the binding, a chain whose
second action refused under one input would retry under the next one, and its
acknowledgment of the first would be reported alongside an effect of the second.

The value is moved rather than copied, which is why nothing here requires a
source's `Output` to be `Clone`. `EcsObserveBuilder::observe` asks for
`PartialEq + 'static`, and that is the whole list.

Two consequences of that order are worth knowing before you rely on a tick:

- **A failing poll fires nothing; a failing action does not undo anything.**
  Every source is polled before any effect runs, so a poll that fails fires
  *nothing* for that tick and returns the first error (phase 2, before any
  `Revision` is committed). Actions then run in sequence, so an action that
  fails returns the first error *after* the actions before it have already taken
  effect. There is no rollback. See [Failure](README-failure.md#failure).
- **Values live in a `NonSend` resource; entities carry only a revision.** Bevy
  requires `Component: Send + Sync + 'static` with no opt-out, and this crate's
  futures are not `Send` on purpose, so a polled value cannot be a component.
  See `docs/bevy-admission.md` §4 for the measurement and the rejected
  alternative.

### Running a tick from async code

`tick` is the synchronous adapter: it drives phase 1 and phase 4 with
`lgwks_std::task::block_on`, which parks the calling thread. That is correct on
a plain thread and a deadlock inside a runtime, because a parked thread cannot
advance the driver its own verbs are waiting on — the timer never fires, the
socket never reports ready, and the sibling task the verb awaits never runs. So:

- **Inside a runtime, `await bot.tick_async()`.** It is the whole tick, in the
  same four phases and the same order. It needs no reactor of its own: the verbs
  are awaited on whatever executor polls it, which is why it is correct on a
  current-thread runtime, on a one-worker runtime, and on `lgwks_std::task`.
- **Outside a runtime, `bot.tick()` still works**, and is the shorter call.
  Called on a thread a runtime already drives it does not park that thread: it
  returns `BotError::TickInsideRuntime` (`docs/guides/lgwks-bot/failures.md`).

Dropping a `tick_async` future — a cancellation, a `select!`, a timeout — is
safe, but it is not the same as a tick that never started. Work the tick had not
reached is untouched, and the entries after it keep their declared order. An
entry whose attempt had *already begun* is held as `TransitionHold::Unrecorded`:
the effect may be live, so it is never attempted again on its own. A later tick
does not replay it — it returns `BotError::PendingTransition` and `Bot::pending()`
names the entry, and `Bot::resolve_effect` is how the caller says what actually
happened. This is the same hold a failed attempt under `EffectIndeterminate`
produces, and it is deliberate: a tick that ends without a record is exactly the
case the write-ahead record exists for.

Futures are **local** (not `Send`): a bot is driven on the calling thread and
domains may hold thread-local state. A non-`Send` future is driven by awaiting
it, or by `lgwks_std::task::join_all` / `join_all_bounded`, which poll many
futures on the calling thread. There is no public API here that *spawns* one: a
single-threaded spawn was the same un-owned handle as the multi-threaded one,
with the added trap that the handle's type could not be named at the call site,
so it is gone for the same reason.
