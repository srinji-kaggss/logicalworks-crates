# Folding the recorder: the semantic tier and what already exists

Pages of this guide: [scope and finding](general-bot-fold.md) · [what is new](general-bot-fold-new.md) · [recorder, settlement, language](general-bot-fold-language.md) · [semantic tier and mapping](general-bot-fold-semantic.md) · [reaching a page](general-bot-fold-reach.md) · [evasion and injection](general-bot-fold-evasion.md) · [sequence 1 to 5](general-bot-fold-sequence.md) · [sequence 6 to 14](general-bot-fold-frontier.md)

### 3.6 The semantic tier — **landed in this change**

§3.5 states a limit rather than hiding it: `"the usual"` cannot be derived from
`"Repeat last order"` by any amount of string distance. That is true, and it is
what a lexicon is for. This section is the layer that answers it, and it is the
one place a model belongs in this design.

`semantic.rs` adds `Embedder` — a seam, not a dependency. The crate still
carries no model, no tensor library and no tokenizer; the weights are not
source, and compiling them in would make every consumer pay for them. A consumer
with a model supplies it. `lgwks_std::similarity` gains `Cosine` alongside the
other metrics, so the comparison itself is not reinvented here.

Three properties make this safe to add to a running system rather than a second
opinion that can disagree with the first.

**It cannot change a verdict the lexicon reached.** The tier is consulted only
when the lexicon returns `Absent`, and every other lexicon verdict — including
`Ambiguous` — is returned verbatim. A model is a tie-breaker of last resort, not
an override: if it could outvote an exact match it eventually would, and an
exact match is the one verdict that cannot be wrong. So for any utterance that
already resolved, enabling this tier is not a behaviour change.

**Its contribution is named.** `MatchTier` gains `Semantic`, so a resolution
says whether a model produced it, and `EmbedderIdentity` carries the model's
name, a content digest of its weights, and its vector width. The digest is what
makes two runs comparable — same name, different weights is a different model —
and it is supplied by the embedder rather than computed here, because only the
code that loaded the weights can know them. A verdict a model produced is only
declarable if the model is named, so the identity travels on the verdict and
from there into the decision receipt — not into a resolver accessor a caller
holding a `Box<dyn Resolver>` cannot reach.

**It reports rather than guesses.** A failed embedder, or one that contradicts
its own declared width, produces `Resolution::Degraded` (§6 item 5) and not
`Absent`. Re-asking is the right response to both, but the record distinguishes
them, so an operator can tell an unclear person from a dependency that is down.

**It says what it decided and on whose authority.** Each accepted answer is
recorded as a versioned `DecisionReceipt` through a required, fallible
`Journal::record_decision`: the session and the flow revision, the node and the
identity of the *ordered* option set, the verdict with its tier and score, the
policy in force, and the model when one was consulted. The transcript is a
rendering of the conversation; the receipt is the audit record, and a sink that
refuses the write aborts the answer rather than leaving a cursor that moved with
nothing to say why.

Three things are deliberate rather than missing. The policy constants
(`0.72`, `0.05`) are **declared, not fitted**, and are constructor arguments
because the right values depend on the model; the margin is small on purpose,
since two phrasings of one option legitimately score within hundredths of each
other. There is **no cache**: an option list is authored and short, and a cache
is a second piece of state needing a bound, an eviction policy and a test for
both — the seam it would sit behind is `Embedder`. And there is **no logging**:
the cause travels in the verdict and the verdict reaches the decision receipt,
which is the run's record, rather than into a second unmanaged copy of the same
fact.

## 4. What maps onto something that already exists

| Described subsystem | This workspace | Note |
|---|---|---|
| REST gateway with JWT and scopes | `Cap` / `GrantSet`, proof-carrying | Different, not strictly stronger. A scope checked once at a gateway is ambient for the rest of the request; `Auth` is re-proved on every call, against the grant set the bot was built with. That set is a **snapshot** (`lib.rs`, `README.md`): `GrantSet` has no revoke operation, so narrowing a running bot needs a mechanism this crate does not have |
| Socket.io stateful multiplexer | `rt::sync` bounded channels | Bounded by construction; `unbounded_channel` is forbidden workspace-wide |
| Browser pool manager, lease/release | `rt::supervise`, `rt::cancel` | A supervised bounded pool is exactly what these already are |
| Playwright Chromium behind a WebSocket | the CDP allow-list in `docs/security-posture.md` | The allow-list is the contract; the driver is an implementation of it |
| Workflow queue (`SKIP LOCKED`, `graphile-worker`) | `rt::task` (`JoinSet` + `CancellationToken`) for in-process; a storefront edge for durable | The durable half is a database client, which is a `lgwks_deps` optional feature, not a new surface |
| Cron / schedule worker | `Bot::tick()` under a `Time<Virtual>` clock | Scheduling is already the tick; §7 of `bot-on-ecs.md` states the bors rule — derive queue membership, never store it |
| Artifact offload to object storage | `domain::fs` plus a storefront edge | The bot hands the payload to a sink; it does not own the sink |
| Webhooks, Sheets, Airtable dispatch | `domain::net`, `domain::notify` | Each is an HTTP+JSON domain impl, not a new architecture |
| The replayer UI (React, rrweb) | **not the bot's job** | The bot is a library. `docs/bot-on-ecs.md` §6 already concluded a console belongs on `gpui` and that the bot must not grow a UI |
| MCP stdio JSON-RPC server | an adapter over the four verbs | An adapter, not a fifth verb. The four-verb contract is `INV-BOT-FOUR-VERBS` and is not negotiable |
| Autonomous agent: page analyzer, indexed handles, screenshot, strict JSON | `domain::chat` + `domain::eval` | See §5 for the two constraints that survive the fold |
