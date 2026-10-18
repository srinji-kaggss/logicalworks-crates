# Folding the recorder: the sequence, steps 1 to 5

Pages of this guide: [scope and finding](general-bot-fold.md) · [what is new](general-bot-fold-new.md) · [recorder, settlement, language](general-bot-fold-language.md) · [semantic tier and mapping](general-bot-fold-semantic.md) · [reaching a page](general-bot-fold-reach.md) · [evasion and injection](general-bot-fold-evasion.md) · [sequence 1 to 5](general-bot-fold-sequence.md) · [sequence 6 to 14](general-bot-fold-frontier.md)

## 6. Sequence

Each step lands green and independently. Steps 1–6 have landed. Next is 7.

1. **The interface model.** ✅ Landed in this change: `interface.rs` —
   `ElementFacts`, the four recognition-vector components, `RecognitionVector`,
   `Recognition`.
2. **Language understanding.** ✅ Landed in this change: `language.rs`, the
   tiered lexicon; `Resolver` reshaped to the three-way `Resolution`, and
   `KeywordResolver` deleted rather than kept beside its superset (§3.5).
3. **The semantic tier.** ✅ Landed in this change: `semantic.rs`, the
   embedding-backed tier over an injected `Embedder`, with `Cosine` added to
   `lgwks_std::similarity`. Consulted only where the lexicon returns `Absent`,
   so it cannot change a verdict the deterministic tiers already reached, and
   the model's identity is recorded with every decision it informs (§3.6).
4. **The locator ladder.** ✅ Landed: `interface.rs` gains `Anchor`, `Ladder`,
   `LadderError`, `ElementFacts::with_anchor` / `anchor`, and
   `RecognitionVector::recognize_with_ladder`, per §3.2. A rung decides by its
   own anchor value: one hit resolves, two or more is `Ambiguous` and stops the
   walk (a weak rung may not "recover" a strong-rung ambiguity), zero hits falls
   through and the final `Absent` carries the strongest near-miss any rung
   scored. `Ladder::new` sorts strongest-first and deduplicates, and rejects an
   empty list the way `Weighted` rejects an empty component set.
5. **The missing typed outcomes.** Three of the same invariant. ✅ Landed:
   `Resolution::Degraded` (§3.6), the verdict a resolver returns when a
   dependency it needs is unavailable — without it, *nothing matched* and *we
   could not look* render identically and asking the person again is the wrong
   repair for the second. And the two halves of the §3.1 defect:
   `BotError::EffectIndeterminate` so a timed-out `Execute` is not retyped
   `Failed` and retried into a duplicate.

   **The second half landed wrong, and the correction is the interesting part.**
   It shipped first as a three-arm `TerminalOutcome` (`Success | Partial |
   Failure`) returned by `Terminal::outcome(unsettled)`, on the reasoning that a
   run which left an effect unsettled is neither a success nor a failure. That
   classifier then turned out to destroy exactly the information it was built to
   carry, in two ways. It mapped every `Refused` terminal to `Failure` whatever
   the unsettled count, so a refused run that had also left an earlier effect in
   doubt reported the refusal and dropped the doubt. And `Partial`'s own
   documentation claimed "part of its output is not known to exist, and part of
   it is", which does not follow from the count it was handed: one attempted
   effect whose response was lost may have produced none at all. `unsettled > 0`
   is uncertainty about *occurrence*, not evidence of partial completion.

   The repair is that the two facts are independent and are stored that way.
   `Outcome` carries a `Disposition` (`Completed | Referred | HandedOff |
   Refused` — the routing class, payload-free) beside an `EffectLedger`, which
   holds `confirmed` and `unsettled` counts. A refusal no longer overwrites
   uncertainty, and neither count is read as evidence of the other.

   That is the same invariant arriving a fourth time, from the opposite
   direction. The first three arrivals were types that could not say a thing;
   this one was a type that said something it had not been told — which is the
   harder failure to notice, because a loss of information leaves a smaller
   surface than a missing one.

   **They are one law, not two patches.** The error says an effect *may* have
   happened; the ledger is where that fact is retained rather than resolved.
   `EffectIndeterminate` changes no consumer's behaviour until something records
   the run it occurred in. Two findings came out of writing it:

   - **The variant, not the cause, has to carry the retry decision.** A consumer
     that parses a cause string to learn whether it may retry will eventually
     get it wrong, and the price is a duplicated merge, message, or process
     launch. So `EffectIndeterminate` is a sibling of `DomainError` rather than
     one variant with a flag: two errors with identical domain *and* identical
     cause text are still distinguishable, which is the property that makes the
     state representable rather than inferred. The regression test asserts
     exactly that pair.
   - **A refusal outranks an unsettled effect.** `Terminal::Refused` stays
     `Failure` however many effects were left unsettled. Folding a decided
     refusal into `Partial` would let it hide in the same class as a possible
     duplicate, and a consumer that must act on a refusal has to see it. This is
     the frontier's rule again — a provisional or decided outcome may not be
     spelled as the other — arriving in the outcome vocabulary.

   **Named honestly, because the step is smaller than it sounds:** there is no
   live producer yet. All three `Execute` domains in this workspace
   (`notify::Slack`, `gh::Merge`, `sys::Process`) are unbound stubs that always
   return `DomainError { cause: "binding required" }`, so nothing today times
   out mid-effect. What landed is the vocabulary and the law; the first domain
   to bind a real request is the one that has to report `EffectIndeterminate`
   rather than `DomainError` on a timeout, and until it does, this closes the
   *type* defect without changing a runtime path.
