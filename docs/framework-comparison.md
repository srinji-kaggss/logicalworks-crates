# Framework comparison

Status: reference. Why the field looks the same, what is actually different, and
what to take. Companion to [`frontier.md`](frontier.md).

## The claim under test

> "It is all fundamentally the same fucking idea."

The survey supports it. Every framework examined — a Rust crawler, a Discord bot
library, two browser-agent SDKs, two scraping frameworks, and two commercial
automation platforms — implements the same five parts:

| Part | What it is |
|---|---|
| **Observe** | Acquire state from a surface |
| **Decide** | Choose what to do next |
| **Act** | Emit an effect |
| **State** | Carry session, queue, cache or variables |
| **Bounds** | Limit the run: budget, capacity, retries, rate |

There is no sixth part anywhere in the field. Once that is seen, the differences
stop looking like architectural disagreement and start looking like four
parameters.

## Four axes, and only four

1. **Observation surface** — HTTP bytes, an event stream, a DOM/accessibility
   tree, pixels, or live form state.
2. **Concurrency model** — parallel-and-stateless with deduplication;
   long-lived connection per shard; in-page on the application's own thread;
   or strictly sequential.
3. **Decision source** — authored rule, fitted model, or model inference.
4. **Authority** — what an action is permitted to do, and who can prove it.

Axes 1 and 3 are where the commercial field competes. Axes 2 and 4 are where the
engineering actually differs — and axis 4 is empty everywhere.

## What each one actually does

### `spider-rs` — crawler, Rust

Concurrency-first, streaming-first. Pages stream out through a **bounded**
subscription channel as they arrive rather than accumulating. Two properties
matter here:

- **Tiered escalation.** HTTP-first, rendering JavaScript *only* on pages that
  need it. Its managed mode "routes through proxies first and escalates to the
  unblocker only on pages that fight back, so you pay for bypass only where it's
  needed."
- **Declared limits.** "Stays inside the limits you set, and stops on its own."

This is the same architecture this project's resolver specification arrived at —
deterministic scorer as the default path, expensive component only on low margin —
reached independently, in a different domain, for the same reason. That is
evidence rather than analogy: two unrelated systems converged on tiered escalation
because the cost argument is the same in both.

### Discord bot frameworks (`discord.py`, `serenity`) — event-driven bots

Five layers: a client owning connection lifecycle; a **gateway** WebSocket for
push events; an **HTTP** client for pull requests with rate-limit enforcement; a
**connection state** object owning the cache and dispatch; and an auto-sharding
client that partitions the event space to scale.

Three ideas worth naming:

- **`intents`** — an enumerated, declared subscription. The runtime delivers only
  event families the bot declared, and a bot that receives an undeclared event
  errors. This is the closest thing to a capability model found anywhere in the
  survey.
- **Dual transport.** Push for events, pull for commands, with different
  guarantees on each.
- **Protocol-level resumption.** `IDENTIFY`/`RESUME` with a heartbeat, so a
  dropped connection continues a session rather than restarting it.

### Stagehand — browser agent SDK

Protocol-first: a browser extension is the in-browser runtime, SDKs speak to it
over CDP with a **single central schema** and JSON-RPC, and the TypeScript,
Python and Go bindings are generated from that one schema for cross-language
parity.

This is structurally the same discipline this workspace already applies — one
schema, generated surfaces, the runtime cordoned behind a protocol boundary. Its
`observe` primitive returns candidate actions and the caller selects among them,
which is the enumerate-then-select shape the resolver needs.

### Crawlee and Scrapy — scraping frameworks

A crawler orchestrates storages, request handlers, retries, concurrency and
coordination. The queue deduplicates by key, concurrency autoscales against
measured CPU and memory, a session pool rotates identity, and retries are bounded
per request.

Two details worth taking:

- Splitting crawler families by **observation surface** (HTTP versus browser)
  rather than by task, with the AI variant bolted on as a subclass rather than
  forked.
- Its LLM crawler "extracts structured data into a **validated Pydantic model**".
  Model output lands in a validated type, never free text. This is the same rule
  as `forge-md-runtime`'s advisory-candidate split, arrived at independently.

### WalkMe and UiPath — by contrast

Neither appears as an engineering framework, because neither publishes one. What
distinguishes them on the four axes is instructive:

- **Observation surface:** live form state, in-page.
- **Concurrency:** none. The rules engine shares the application's own JavaScript
  thread, so every check competes with the application for the frame budget.
- **Decision source:** authored rule.
- **Authority:** ambient, unbounded.

**This is the direct answer to "WalkMe is too slow."** It is not slow because of
models. It is slow because it has neither of the two bound mechanisms the
crawlers treat as foundational — no declared limits with self-termination, and no
tiered escalation — and because it runs on the application's own thread instead
of beside it. The crawlers solved the identical problem and called it
architecture.

## What is worth taking

Ranked by expected value to this project.

**1. Declared observation scope, borrowed from Discord's `intents`.** The bot has
`GrantSet` bounding what an *action* may do, and nothing bounding what it may
*observe*. A declared, enumerated observation intent the runtime enforces — where
capturing an undeclared surface is an error rather than a silent success — closes
the read side of the same gap. This is the single most valuable idea in the
survey and it has no equivalent in the estate today.

**2. Tiered escalation with a measured firing rate.** Already adopted for the
resolver; `spider-rs` confirms it generalises, and the same shape should apply to
re-observation and to journal compaction.

**3. Bounds as first-class, not as configuration.** Capacity on every channel,
deduplication by key, bounded retries, and a run that stops on its own. The
frameworks that scaled all did this; the platforms that did not are the slow
ones.

**4. Model output into a validated type, never free text.** Enforced twice
independently — Crawlee's validated model, `forge-md-runtime`'s advisory
candidate.

**5. One schema, generated surfaces, a cordoned runtime.** Stagehand's shape, and
already this workspace's shape.

## The gap none of them close

Every framework surveyed bounds **how much** work is done and none bounds **what
an action may do**. Discord's `intents` scopes what a bot *receives*; nothing in
the field scopes what it *sends*, carries proof that a specific invocation was
authorized, or records a decision with provenance a third party can verify.

That is not an accident of this sample. The commercial platforms cannot
differentiate on authority because their value proposition is unconstrained
automation, and the open frameworks have not needed it because their users are
the operators.

So the honest conclusion is: **the thesis is correct.** This is one idea,
differentiated on observation surface and decision source, packaged and sold on
hosting and bypass infrastructure. The two axes that would carry real
differentiation — authority bounded at the invocation, and decision provenance
that survives third-party verification — are the two nobody occupies.

Which is a better position than it sounds. It means the bot does not need to
invent a new category to be different. It needs to be the first implementation of
the *same* idea that bounds itself and can prove what it did.
