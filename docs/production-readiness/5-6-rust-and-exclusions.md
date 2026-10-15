# 5. Why Rust, without the adjectives, and 6. What is deliberately excluded

Part of [production readiness](../production-readiness.md), §5 and §6.

## 5. Why Rust, without the adjectives

"Blazing fast and safe" is a marketing sentence. Here is what the language
actually contributes, and what it does not.

**Speed, relative to the RPA category.** The commercial guidance platforms run
their rules engine on the application's own JavaScript thread. That is the
direct answer to "WalkMe is too slow", and it is an architectural defect rather
than a tuning problem: every check competes with the host application for the
frame budget. `lgwks_bot` runs beside the process and observes it. Flow
engines (Node-RED, n8n) pay interpreter dispatch on every decision; `Evaluate`
here is a monomorphised Rust closure. JVM and .NET automation platforms pay GC
pauses at exactly the wrong moments — during a UI wait — and Rust has no collector.
Those are category-level differences and they compound.

**Speed, relative to a for-loop.** Slower, by 118x to 2 665x. Stated above.

**Safety, mechanically rather than aspirationally.**

- `forbid(unsafe_code)` at workspace scope. The language will not compile a
  data race into existence in safe Rust, and this workspace does not permit the
  escape hatch.
- Capability is a value. `Auth` is minted only by `GrantSet::issue`, travels
  with the call, and is checked at build and on every invocation. In every
  rival in §3, authority is a token in a config file or an ambient global.
- `forbid`, not `deny`, on the anti-pattern lint set, because `deny` loses to a
  single `#[allow]` and `forbid` is `E0453`.
- Exhaustive enums, `#[non_exhaustive]` on public surfaces, typed error
  hierarchies with `source` chains rather than `String`.
- Bounded channels only; `JoinSet` and `CancellationToken` tracked for every
  spawn; every loop has a bound.
- A condition language with a proved termination bound, which a language with
  iteration does not have (T7 is the theorem that says so).

**What Rust does not give you.** Correctness of the model. Memory safety is not
crash safety, is not protocol safety, and is not "the effect did not fire
twice." Those are the effect kernel's job and §4.6 says how far that job has
actually been verified.

## 6. What is deliberately excluded from this evaluation

Following the rig's own convention, the exclusions are named with their
direction of bias.

- **No comparison against commercial RPA on coverage.** UiPath's connector
  catalogue beats anything here by orders of magnitude. Excluding it favours
  this crate. It is excluded because counting connectors is not an engineering
  comparison, and because the claim being tested is about the mechanism, not the
  catalogue.
- **No human usability measurement.** [`async-sdk-shape.md`](../async-sdk-shape.md)
  states the contract — a task author supplies the script, not the machinery.
  #87 implemented it (closed 2026-10-03), and `bench/ai-authoring/runs/` holds
  324 fixed-model trials across six model runs plus 38 dry and mutant control
  trials (`results.jsonl` line counts), including the four-arm comparison against
  `futures` that §4.1 scores. A fixed model is not a person (INV-BOT-141), so
  human authoring time is still unmeasured, and excluding it favours this crate.
- **No cost-per-outcome numbers.** RPA is bought on cost per successful
  transaction. Nothing here measures that.
- **No AI-in-the-loop comparison.** The claim is scoped to non-full-AI
  automation. A model in the decision source is axis 3 of the survey and is
  deliberately out of scope here.
- **No third-party audit.** `proofs/` is a design proof by the author. The
  security posture document states what a reviewer can check without trusting
  the vendor; that is weaker than an external audit.
