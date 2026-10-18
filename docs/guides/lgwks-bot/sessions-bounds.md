# Guidance sessions: bounds and what a finished run reports

Pages of this guide: [the flow and its pieces](sessions.md) · [bounds and outcomes](sessions-bounds.md)

## The bounds

`FlowBounds::new(budget)` caps runtime steps including answer attempts, and
`FlowSpec::validate` refuses a node count that exceeds the declared budget
(`BotError::FlowBudgetExceeded`). The runner charges each step against the same
budget (`crates/lgwks-bot/src/session/runner.rs:435`) and returns
`BotError::SessionBudgetExceeded`, so a flow whose graph lets the cursor loop
still terminates.

`Session::steps()` reports the charged count, which is what you compare against
your own bound.

### Bytes, which the step budget does not bound

A step budget bounds how many times something happens and not how large it is,
and the two are independent: one `say` node is one step whether it says four
bytes or four gigabytes, and a document that names `${answer}` twenty thousand
times is a two-hundred-kilobyte file whose expansion is a hundred and sixty
megabytes. `Session` therefore enforces four byte ceilings as well
(`crates/lgwks-bot/src/session/limits.rs:16`):

- `MAX_UTTERANCE_BYTES` — the bytes accepted in one answer, checked in
  `Session::answer` before the step is charged and before the utterance is
  copied anywhere, so a refused line consumes no budget and leaves no record.
- `MAX_VALUE_BYTES` — the bytes one stored variable value occupies when
  rendered. Checked at load, against every candidate of every ask, so a
  candidate the session could never store is refused before anyone is asked it
  (`BotError::AskOptionTooLarge`).
- `MAX_RECORD_BYTES` — the bytes of one rendered record payload.
- `MAX_SESSION_BYTES` — the total bytes one session retains across its whole
  transcript and visited path, which is what bounds a conversation the graph
  lets repeat.

Expansion is bounded at the *computed* size rather than the built one. A
template is compiled once into literal and placeholder parts, and the sum of the
parts is accumulated with `checked_add` before any output buffer exists; a
template whose computed expansion exceeds the record ceiling is refused with
`BotError::TemplateExpansionTooLarge`, and arithmetic that would overflow is the
same refusal at a larger size. A template whose *literal* bytes alone exceed the
ceiling is refused when the document loads, because no value can shrink it.

`FlowBounds::resources` lets a document declare ceilings of its own, and
`Session::with_limits` lets an operator declare theirs. Neither can widen the
other: an axis takes the smaller of the two, a request above the shipped ceiling
is refused (`BotError::ResourceLimitAboveCeiling`) rather than clamped, and the
ceiling in force is what `Session::limits()` reports. `Session::retained_bytes()`
reports the aggregate charged so far.

## What a finished run reports

`Terminal` records how the run ended: `Completed`, `Referred { target }`,
`HandedOff { target }`, or `Refused { reason }`.

Which one a node produces is computed in exactly one place,
`FlowSpec::effective_terminal`, and validation and execution both read it. An
`End` node completes unless the terminal map declares something else, and every
declared outcome is a genuine override there — that is how a flow refuses
(`Terminal::Refused`) rather than ending quietly. A `Handoff` or `Refer` node
already names its target, so the only declaration it accepts is the one that
repeats that outcome; one that contradicts it, whether a different target or a
refusal, is refused at load with `BotError::ConflictingTerminalDeclaration`
(`crates/lgwks-bot/src/session/validate.rs:528`). A document that says a handoff is not
authorized therefore never runs as a handoff, which is what a consumer
dispatching on the returned disposition depends on.

`Terminal::outcome(EffectLedger) -> Outcome` (`crates/lgwks-bot/src/session/graph.rs:278`)
carries two independent facts through unchanged, and that is the whole of the
method:

- the `Disposition`, one of `Completed`, `Referred`, `HandedOff`, `Refused`
  (`crates/lgwks-bot/src/session/graph.rs:120`);
- the `EffectLedger`, a `confirmed` count and an `unsettled` count
  (`crates/lgwks-bot/src/session/graph.rs:147`).

It does not classify, and the reason is in the source: an earlier version
returned a single enum and had to choose, for a refused run that also left an
effect unsettled, whether to report the refusal or the uncertainty. Both are
true, and a report that drops either one is wrong for whoever needed it.

`EffectLedger::needs_reconciliation()` reads the count and never the
disposition, so it stays true for a refused run that left an effect in doubt.
A non-zero `unsettled` does not imply that anything succeeded either. One
attempted effect whose response was lost may have produced zero effects or one,
which is why the type says "nothing is confirmed, and one is in doubt" rather
than "part of the output exists".

`Outcome::disposition()` and `Outcome::effects()` are how you read the two back
out. Route on the disposition; reconcile on the ledger.

Validation is not execution. `FlowSpec::validate` proves the graph is
well-formed: reachable nodes, declared variables, resolvable transitions, a
budget that fits. It does not prove the flow asks the right questions. See
[resolution](resolution.md) for the boundary between a resolver selecting an
outcome and the outcome being correct.
