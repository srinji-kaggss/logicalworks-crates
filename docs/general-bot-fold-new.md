# Folding the recorder: what is genuinely new

Pages of this guide: [scope and finding](general-bot-fold.md) · [what is new](general-bot-fold-new.md) · [recorder, settlement, language](general-bot-fold-language.md) · [semantic tier and mapping](general-bot-fold-semantic.md) · [reaching a page](general-bot-fold-reach.md) · [evasion and injection](general-bot-fold-evasion.md) · [sequence 1 to 5](general-bot-fold-sequence.md) · [sequence 6 to 14](general-bot-fold-frontier.md)

## 3. What is genuinely new

Four things have no artefact here yet. Everything else in §4 is either already
present or a domain implementation.

### 3.1 The interface model — **landed in this change**

`docs/guidance-runner-spec.md:193` deferred *"the interface model (element
entities, recognition-vector components, acquisition systems) on the ECS
substrate"*, blocked on Workstream A. Workstream A has landed
(`lgwks_std::similarity`: `EditDistance`, `Jaccard`, `PathSimilarity`,
`Geometry`, `Weighted`), so the first two thirds are now implementable and are
implemented:

- `ElementFacts` — element entities: the facts a snapshot reports about one
  element, with identity (`tag`, `identity`, `frames`) separated from mutable
  content (`text`, `bounds`), which is the split `docs/bot-on-ecs.md` §4 measured
  as load-bearing.
- `IdentityComponent` / `PathComponent` / `TextComponent` / `GeometryComponent` —
  recognition-vector components: adapters projecting one field of `ElementFacts`
  onto a `lgwks_std::similarity` metric, so heterogeneous metrics compose in one
  `Weighted`.
- `RecognitionVector::fingerprint()` — the shipped opinion: weights `1/2`
  identity, `1/4` path, `1/8` text, `1/8` geometry. Powers of two, so they sum to
  exactly `1.0` in binary floating point rather than by rounding luck.
- `Recognition` — the verdict.

Two facts are decided before scoring rather than weighted: an element in another
document, and an element of an incompatible tag. A fact that is absent on either
side — no identifying attribute, no text, no structural path — contributes
nothing, rather than the `1.0` its metric returns for the empty-vs-empty case.
The weights are not renormalized around the gap, so identity and path together
being `3/4` is what makes them required for the `3/4` threshold to be reachable.
GitHub issue #39 filed this: two snapshot fields that are missing on *both*
sides were scoring as agreement, which let an unrelated element with a different
tag resolve against a target that had nothing to identify it by.

`RecognitionVector::recognize` returns `Resolved` / `Ambiguous` / `Absent`, and
**this is the load-bearing decision in the whole fold.** The described platform
resolves an element to a single best candidate by similarity distance. A
best-candidate resolver has no way to report *"two elements matched equally
well"*: it returns the first, and the run either clicks the wrong control or
clicks nothing and reports a timeout. `Ambiguous` makes that state
representable, and the margin requirement makes it reachable — the best
candidate must lead the runner-up by a declared amount or the caller must
re-ask rather than commit.

This is the same invariant the workspace has now reached three times:

| Arrival | Statement |
|---|---|
| `docs/bot-on-ecs.md` §8.1 | a single-bit seen-set cannot distinguish "done" from "never started" |
| `BotError` | no variant distinguishes "the effect did not happen" from "it may have happened" |
| this module | no two-way verdict distinguishes "nothing matched" from "several matched" |

One invariant, three arrivals. The first and third landed here; the second
landed in §6 step 5, and the two halves it turned out to have are one law rather
than two patches.

### 3.2 The locator ladder and selector synthesis

The described platform generates, for each selected element, an ordered ladder —
stable `id`, then `data-testid`, then ARIA role and accessible name, then visible
text, then a class path — and falls back down it.

This slice is landed (`interface.rs`, step 4 below). A ladder
must be walked against candidates, which means each candidate must offer the
anchors the ladder names; `ElementFacts` does not carry them yet, and shipping a
ladder that cannot be joined to its candidates would be a half-built path of
exactly the kind the one-path rule forbids. The shape is settled, though, and it
is small:

- `Anchor` — a closed enum (`Id`, `TestId`, `Role`, `Text`, `ClassPath`) with an
  explicit `rank()`. Explicit rather than derived `Ord`, because a derived order
  puts the strongest variant first *or* last depending on declaration order, and
  that is a coin-flip a reader should not have to resolve.
- `Ladder` — anchors sorted strongest-first, deduplicated, constructible only
  non-empty (an empty ladder is an error, matching `Weighted`'s treatment of an
  empty component set).
- `RecognitionVector::recognize_with_ladder` — walk strongest-first, and return
  the *strongest* non-resolved verdict observed. An `Ambiguous` at a strong
  anchor must not be silently retried at a weak one: a weak anchor that succeeds
  where a strong one was ambiguous is a likely mis-match, not a recovery.
