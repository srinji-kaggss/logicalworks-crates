# Use cases and boundaries

What each crate is for, what none of them is, and where they sit beside the
crates a search finds first. Moved from the repository README, which keeps
only the quickstart.

## Use cases

### Automation that runs unattended

- A bot that watches an endpoint, a file, or a queue and acts when something
  changes, rather than on every timer tick. → `lgwks_bot`
- A webhook- or cron-driven job that retries with backoff and is safe to kill
  mid-flight. → `lgwks_bot` for the loop, `lgwks_std::retry` for the policy
- Long-running background work that cancels cleanly on shutdown, without a
  leaked task or a loop that never ends. → `lgwks_bot::rt::supervise`

### Giving an agent real authority, safely

- An LLM-driven or rule-driven agent that must not be able to do more than you
  granted it. Every verb checks a capability proof before it acts, so "what can
  this agent touch?" is answerable from its grant set rather than from a reading
  of the code. → `lgwks_bot`. You bring the model; this is the execution
  substrate, not a model client.

### HTTP and data without an async runtime

- Calling an HTTP API from a CLI, a build script, or a synchronous service, where
  pulling in a whole async runtime is the wrong trade. → `lgwks_std::http` is
  blocking, rustls-only, and needs no tokio
- Parsing JSON that may be truncated, streamed, or hostile. → `lgwks_std::json`
  reports the offset it stopped at, so a partial body is a value rather than an
  error you have to guess your way out of
- Config in RON, or a compact binary wire format. → `lgwks_std::ron`,
  `lgwks_std::wire`

### Tools that read source code

- Building a linter, a codemod, a repository audit, or an "explain this file"
  tool across several languages. → `lgwks_ast` gives one AST type and one
  diagnostic type across every grammar you enable
- Reporting a parse failure with a real span. → `lgwks_ast` diagnostics carry
  the recovery-node kind and bounded byte ranges into the original source

### Logging you can query

- Structured, levelled logs from a library without `println!`, plus a default
  SDK debugger bootstrap that can render compact local lines or JSON. →
  `lgwks_std`, feature `trace` (default on)

### A dependency graph you can defend

- Answering "what third-party code is in our build, and who approved it?" with a
  command instead of an archaeology project. → `lgwks_deps`, and `lgwks-deps check`

## What these crates are not

- **Not an agent framework.** `lgwks_bot` runs your logic and gates its
  authority; it does not call a model for you.
- **Not an async runtime.** `lgwks_bot::rt` is a curated surface over tokio,
  which owns the engine. Parity and the deliberate divergences are documented in
  [`docs/async-parity.md`](async-parity.md).
- **Not a telemetry backend.** `lgwks_std::trace` re-exports `tracing` and ships
  the default local subscriber bootstrap. OTLP export, collectors, credentials,
  and retention stay at the deployment boundary.
- **Not a parser generator.** `lgwks_ast` wraps tree-sitter grammars; it does not
  produce them.

## Relationship to adjacent crates

The crates a reader is most likely to encounter first when searching these
problems, and where the boundary between them lies.

| If you're looking for | You'll find first | How this differs |
|---|---|---|
| An LLM agent framework | Rig, ADK-Rust, AutoAgents, Metalcraft, neuron, anda | Those own model integration: providers, prompts, tool schemas, orchestration. `lgwks_bot` owns none of that. It is the layer beneath: what the agent is permitted to do, and a loop that terminates on request. The two compose. |
| Zero-trust agents | Symbiont, ZeroTrustAgent, agentmesh | The same premise at a narrower boundary. See [Agent security](#agent-security). |
| Background job processing | fang, backie, apalis, tokio-cron-scheduler | Those are queue- and database-backed workers. `lgwks_bot` provides bounded in-process supervision, for work owned by the process rather than by a broker. |
| Multi-language parsing | tree-sitter-language-pack, rust-code-analysis, ast-grep | The same grammars. `lgwks_ast` narrows the surface to one `Language` enum, one AST type, and typed diagnostics carrying spans, selectable by cargo feature so only the shipped grammars compile. |
| Blocking HTTP without an async runtime | `ureq`, `reqwest::blocking` | `lgwks_std::http` is the same approach with rustls-only TLS and no tokio, alongside JSON, retry, and time, so a CLI depends on one crate rather than five. |

### Agent security

Agent security practice has converged on a shared vocabulary: deny by default,
runtime gating, tool allowlists, task-scoped permissions. `lgwks_bot`
implements the first two under those names.

- **Runtime gating.** Every verb invokes `Auth::check(&self.required_caps())?`
  before it touches anything, so an unauthorized call is refused before the
  effect rather than after it.
- **Deny by default.** A proof covering no capability authorizes nothing. A
  vacuous proof presented against `bot.net` is denied; that case is covered by
  test.

It does not implement short-lived credentials, MCP tool routing, human approval
gates, or isolated enclaves. Those are absent, not partially built.

### In-process background work

The two conventional options for Rust background work are a fire-and-forget
`tokio::spawn`, for short-lived tasks that do not survive a restart, and a
database- or broker-backed queue for work that must.

Neither covers the common case: work that is in-process, long-lived, and
required to stop cleanly on shutdown. `lgwks_bot::rt::supervise` is that middle.
Concurrency is bounded, tasks are tracked rather than detached, and a cancelled
loop stops without waiting on a body that will never observe the cancellation.
