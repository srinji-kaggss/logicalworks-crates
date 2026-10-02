# Framework comparison

Status: reference. Why the field looks the same, what is actually different, and
what to take. Companion to [`frontier.md`](frontier.md).

## Primary sources

Every external project named in this page was re-checked on **2026-10-02**
against the project's own repository, release notes or documentation, pinned to
the version shown. A claim the project itself does not support is marked
*unsourced* rather than deleted. Nothing here is a benchmark or measurement of
this crate.

| Project | Version at 2026-10-02 | Primary source |
|---|---|---|
| `spider-rs` | `v2.52.2` | repo `README.md` + release tag |
| `discord.py` | `2.7.1` | PyPI metadata + `intents.html` docs |
| `serenity` | `v0.12.5` | repo release tag |
| Stagehand | npm `@browserbasehq/stagehand@3.7.3`; docs/Go SDK `v4` | repo `README.md` |
| Crawlee (JS) | `@crawlee/core` `3.18.2` | npm registry + repo `README.md` |
| Crawlee (Python) | PyPI `crawlee` | repo `README.md` |
| Scrapy | `2.19.0` | release tag + repo `README.md` |
| WalkMe, UiPath | n/a | no public framework released |

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
  unblocker only on pages that fight back, so you pay for bypass exactly when
  it's needed and never when it isn't" (`spider-rs` `README.md` at tag `v2.52.2`).
  The tail of the earlier quote here ("only where it's needed") is the current
  `main` wording, not the release's.
- **Declared limits.** "The crawler discovers links, respects boundaries, and
  stops on its own" (same README at `v2.52.2`; the earlier "stays inside the
  limits you set" is `main`'s wording again). The subscription channel is bounded
  by a capacity the caller passes to `website.subscribe(<n>)`.

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

- **`intents`** — an enumerated, declared subscription: the bot names the event
  buckets it wants and the gateway delivers those (`discord.py` `2.7.1`,
  `intents.html`). *Unsourced:* "a bot that receives an undeclared event errors"
  — the guide describes subscription buckets and privileged intents, but states
  no such error. This remains the closest thing to a capability model found
  anywhere in the survey.
- **Dual transport.** Push for events, pull for commands, with different
  guarantees on each.
- **Protocol-level resumption.** `IDENTIFY`/`RESUME` with a heartbeat, so a
  dropped connection continues a session rather than restarting it.

### Stagehand — browser agent SDK

Protocol-first: a browser extension runs next to the browser as the in-browser
runtime, and the TypeScript, Python and Go bindings are generated from one schema
for cross-language parity (`browserbase/stagehand` `README.md`; current docs are
`v4`, latest npm release tag `@browserbasehq/stagehand@3.7.3`). *Unsourced at this
version:* "SDKs speak to it over CDP with ... JSON-RPC" — the pinned README shows
one browser driver and one generated surface, but names neither CDP nor JSON-RPC.

This is structurally the same discipline this workspace already applies — one
schema, generated surfaces, the runtime cordoned behind a protocol boundary. Its
`observe` primitive returns candidates (selectors) and the caller selects among
them, which is the enumerate-then-select shape the resolver needs.

### Crawlee and Scrapy — scraping frameworks

A crawler orchestrates storages, request handlers, retries, concurrency and
coordination. Crawlee (JS `3.18.2` / Python) states exactly these: automatic
scaling "with available system resources", integrated "proxy rotation and session
management", and configurable, bounded retries (`README.md`). The dedup-by-key
queue and the rotating session pool are Crawlee features; they are **not** Scrapy
core — Scrapy's `AutoThrottle` scales against observed server latency, not local
CPU/memory, and a rotating-proxy session pool is a third-party plugin — so
reading that sentence as Scrapy's design is *unsourced*.

Two details worth taking:

- Splitting crawler families by **observation surface** (HTTP versus browser)
  rather than by task, with the AI variant bolted on as a subclass rather than
  forked.
- *Unsourced:* an LLM crawler that "extracts structured data into a **validated
  Pydantic model**". Neither the pinned Crawlee (JS) nor Crawlee for Python
  `README.md` names an LLM crawler or Pydantic extraction. The underlying rule —
  model output into a validated type, never free text — is the same as
  `forge-md-runtime`'s advisory-candidate split, arrived at independently.

### WalkMe and UiPath — by contrast

Neither appears as an engineering framework, because neither publishes one, so
everything below is an assessment with no primary source to cite. What
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
