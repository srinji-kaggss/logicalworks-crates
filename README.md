# logicalworks-crates

Four independently versioned Rust crates: a zero-dependency core, a runtime for
long-running automation, a multi-language AST front end, and an audited
dependency storefront. Each is usable on its own. Take one and ignore the rest.

They share a workspace and a release process, not a dependency graph. Only
`lgwks_bot` depends on `lgwks_std`, and nothing depends on `lgwks_ast`.

**Contents**

- [Crates](#crates)
- [Quickstart](#quickstart)
- [Use cases](#use-cases)
- [What these crates are not](#what-these-crates-are-not)
- [Relationship to adjacent crates](#relationship-to-adjacent-crates)
- [Agent security](#agent-security)
- [In-process background work](#in-process-background-work)
- [Choosing a crate](#choosing-a-crate)
- [Documentation](#documentation)
- [Building and testing](#building-and-testing)
- [Contributing](#contributing)
- [License](#license)

## Crates

| Crate | Version | Docs | What it gives you |
|---|---|---|---|
| `lgwks_std` | 0.6.7 | [docs.rs](https://docs.rs/lgwks_std) | Everyday primitives with no async runtime required: JSON/RON/wire codecs, a blocking HTTP client, retry policies, structured logging, time and calendars, hashing, ids, globs, process control |
| `lgwks_bot` | 0.5.0 | [docs.rs](https://docs.rs/lgwks_bot) | A runtime for bots that run for weeks: four verbs (Observe, Evaluate, Execute, Query), capability-gated authority, change-triggered execution, and supervision that bounds background work |
| `lgwks_ast` | 0.2.2 | [docs.rs](https://docs.rs/lgwks_ast) | Parse many languages into one AST type. tree-sitter grammars behind cargo features, bounded traversal, and typed diagnostics for tools that report on code |
| `lgwks_deps` | 0.1.13 | [docs.rs](https://docs.rs/lgwks_deps) | One audited place to opt into third-party stacks, plus `lgwks-deps check` to prove no dependency entered your build unreviewed |

Versions move independently from one repository and one tag. The table lists the
versions in this tree's manifests. A manifest version is not an upload: the
newest tags are recorded in [`docs/guides/index.md`](docs/guides/index.md), and
[`docs/releasing.md`](docs/releasing.md) keeps the two apart.

> **Names.** Package `lgwks_std` (underscore) lives in directory
> `crates/lgwks-std` (hyphen) and is imported as `use lgwks_std::...`. The one
> exception is the gate binary `lgwks-deps` (hyphen), built from the library
> `lgwks_deps` (underscore): `cargo install lgwks_deps` for the CLI,
> `cargo add lgwks_deps` for the storefront library.

## Quickstart

```sh
cargo new demo && cd demo
cargo add lgwks_std --features json,http
cargo add lgwks_bot
```

```rust
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Output goes through an explicit locked handle, which turns a broken pipe
    // (`demo | head`) into an ordinary `Err` instead of a panic.
    let mut out = std::io::stdout().lock();

    // Core primitives: zero-config, zero external deps by default.
    let now = lgwks_std::time::now_rfc3339();
    writeln!(out, "now: {now} hex: {}", lgwks_std::hex::encode(b"hi"))?;

    // bot: capability-gated actors. Grants are required to build, not only to run.
    // `build` returns a typed error; the caller decides what to do with it.
    let bot = lgwks_bot::Bot::builder("demo").build(&lgwks_bot::GrantSet::empty())?;
    let _ = bot;

    Ok(())
}
```

The snippet above is compiled by the workspace gate, from
[`crates/lgwks-bot/examples/quickstart.rs`](crates/lgwks-bot/examples/quickstart.rs).

The dependency gate reads a project's lockfile and its committed register, so it
runs as a command against a project rather than from a library call:

```sh
cargo generate-lockfile   # the gate reads Cargo.lock
cargo install lgwks_deps
lgwks-deps init .         # write a fail-closed register
lgwks-deps check .        # audit, and refuse any unowned external edge
```

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
  file, range, and severity

### Logging you can query

- Structured, levelled logs from a library without `println!` and without
  committing your consumers to a subscriber. → `lgwks_std`, feature `trace`
  (default on)

### A dependency graph you can defend

- Answering "what third-party code is in our build, and who approved it?" with a
  command instead of an archaeology project. → `lgwks_deps`, and `lgwks-deps check`

## What these crates are not

- **Not an agent framework.** `lgwks_bot` runs your logic and gates its
  authority; it does not call a model for you.
- **Not an async runtime.** `lgwks_bot::rt` is a curated surface over tokio,
  which owns the engine. Parity and the deliberate divergences are documented in
  [`docs/async-parity.md`](docs/async-parity.md).
- **Not a logging facade.** `lgwks_std::trace` re-exports `tracing`; bring your
  own subscriber.
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

## Choosing a crate

| I need... | Reach for | Feature |
|---|---|---|
| Hex, base64, timestamps, UUIDs, hashing, glob, LEB128 | `lgwks_std` core | default, zero deps |
| Retry budgets (attempts, backoff, deadlines) | `lgwks_std::retry` | default, zero deps |
| Regex, JSON, RON, binary wire, HTTP client, reachability | `lgwks_std` | `pattern`, `json`, `ron`, `wire`, `http`, `online` |
| `block_on`, bounded `join_all`, `spawn_blocking` (sync) | `lgwks_std::task` | default, zero deps |
| Capability-gated automation (Observe/Evaluate/Execute/Query) | `lgwks_bot` | default (sync) |
| Async timers, channels, sockets, processes, signals | `lgwks_bot::rt` | `rt`, `time`, `sync`, `net`, `process`, `fs`, `signal` |
| Parse Rust/Python/TS/JS/Go/Java/Swift in a tool | `lgwks_ast` | default (7 grammars) |
| Rust source lint scan (zero-gate detectors) | `lgwks_deps` | `scan` (default for CLI) |
| Raw `tokio` engine without the bot facade | `lgwks_deps` | `tokio*` with `default-features = false` |
| GPU desktop UI (Zed's GPUI) | `lgwks_deps` | `gpui` with `default-features = false` |
| Native terminal UI (AppCUI) | `lgwks_deps` | `appcui` with `default-features = false`, version 0.1.10 or newer |

Three behaviours are worth knowing before you depend on them, and each is
documented where it applies. `online` is feature-gated and is not part of
`core`. `tempfile` is a development-only edge, so there is no production
temporary-directory path. Typed errors derive through `lgwks_ast`
(`extern crate lgwks_ast as thiserror`) rather than `lgwks_std`. The
`join!`/`select!`/`try_join!` re-exports in `lgwks_bot` require the `macros`
feature, and there is deliberately no `#[tokio::main]` equivalent: entry is
through `Runtime::block_on`.

## Documentation

Depth lives in [`docs/`](docs/), not in this file. If you are here to use a
crate rather than to change one, start with the [consumer guides](docs/guides/index.md);
the rest of this table is design history and contributor policy.

| Document | Covers |
|---|---|
| [`docs/guides/index.md`](docs/guides/index.md) | Choose a crate, and which symbols are stable versus main-only. |
| [`docs/guides/lgwks-bot/index.md`](docs/guides/lgwks-bot/index.md) | `lgwks_bot`: purpose, non-goals, and the supported versions. |
| [`docs/guides/lgwks-bot/getting-started.md`](docs/guides/lgwks-bot/getting-started.md) | One source, one condition, one effect, compiled and run. |
| [`docs/guides/lgwks-bot/features.md`](docs/guides/lgwks-bot/features.md) | Every `lgwks_bot` feature, and what `--no-default-features` does not remove. |
| [`docs/guides/lgwks-bot/authority.md`](docs/guides/lgwks-bot/authority.md) | Admission, the `Auth` snapshot, and the no-sandbox boundary. |
| [`docs/guides/lgwks-bot/failures.md`](docs/guides/lgwks-bot/failures.md) | Poll failure, partial action failure, and unknown effect. |
| [`docs/guides/lgwks-bot/background-work.md`](docs/guides/lgwks-bot/background-work.md) | Bounded fan-out, supervision, and the cancellation limits. |
| [`docs/guides/lgwks-bot/sessions.md`](docs/guides/lgwks-bot/sessions.md) | Unreleased: validated guidance flows and the session runner. |
| [`docs/guides/lgwks-bot/resolution.md`](docs/guides/lgwks-bot/resolution.md) | Unreleased: lexical and semantic resolution, and degraded verdicts. |
| [`docs/guides/lgwks-bot/domains.md`](docs/guides/lgwks-bot/domains.md) | Unreleased: the domain registry, and what a spec's identifiers resolve to. |
| [`docs/guides/lgwks-std/index.md`](docs/guides/lgwks-std/index.md) | `lgwks_std`: install, feature selection, and logging. |
| [`docs/guides/lgwks-ast/index.md`](docs/guides/lgwks-ast/index.md) | `lgwks_ast`: install, grammar selection, bounds, and diagnostics. |
| [`docs/guides/lgwks-deps/index.md`](docs/guides/lgwks-deps/index.md) | `lgwks_deps`: library use versus CLI and policy adoption. |
| [`docs/dependency-doctrine.md`](docs/dependency-doctrine.md) | How a dependency is admitted, the replacement matrix, and where no equivalent exists. Read this before adding a crate. |
| [`docs/async-parity.md`](docs/async-parity.md) | The `lgwks_bot::rt` surface against tokio and the alternatives: the capability matrix, the open gaps, and the deliberate divergences. |
| [`docs/async-sdk-shape.md`](docs/async-sdk-shape.md) | The async surface as an SDK: its shape, its invariants, and how to migrate. |
| [`docs/declarative-orchestration.spec.md`](docs/declarative-orchestration.spec.md) | The task-first front door: typed task, host and report, one registry and execution path, the responsibility split, and the full known-deficit repair. Proposed, not shipped. |
| [`docs/orchestration-lifecycle.spec.md`](docs/orchestration-lifecycle.spec.md) | Single-owner state, atomic fingerprint and value commits, attempt-bound evidence, readiness and process-tree ownership, and the honest durability boundary. |
| [`docs/ai-orchestration.spec.md`](docs/ai-orchestration.spec.md) | AI-authored scripts and AI workloads: constrained proposals, bounded artifact context, subject pinning, no-progress intervention and same-model evaluation. |
| [`docs/pr-review-orchestration.spec.md`](docs/pr-review-orchestration.spec.md) | The reference journey: a pinned snapshot through script, checked publication and lost-response reconciliation, and its explicit non-CAS freshness limit. |
| [`docs/orchestration-acceptance.spec.md`](docs/orchestration-acceptance.spec.md) | The 36 proposed public-interface falsifiers, and the developer-experience and quality evaluation that would accept the facade. Every case unrun. |
| [`docs/orchestration-evidence.md`](docs/orchestration-evidence.md) | The pinned source findings, the foundational Rust assessment, and the reference entries the orchestration specs are built on. |
| [`docs/bot-on-ecs.md`](docs/bot-on-ecs.md) | Bot semantics on an ECS substrate, and what that mapping costs. |
| [`docs/bevy-admission.md`](docs/bevy-admission.md) | The Bevy ECS decision and the alternatives measured against it. |
| [`docs/candle-admission.md`](docs/candle-admission.md) | ML inference admission: authority, transitive surface, verification. |
| [`docs/appcui-admission.md`](docs/appcui-admission.md) | Native terminal UI admission: authority and verification. |
| [`docs/distributed-boundaries.md`](docs/distributed-boundaries.md) | What these crates do and refuse on a distributed network path. Read it before placing them on one. |
| [`docs/general-bot-fold.md`](docs/general-bot-fold.md) | How a full browser recorder-and-replayer architecture folds into this bot: what already is it, what becomes a seam, and what is refused. |
| [`docs/security-posture.md`](docs/security-posture.md) | The security case: the execution-locus claim and its ceiling, what the browser integrity stack does and does not prove, and the evidence a reviewer can check without trusting the vendor. |
| [`docs/guidance-runner-spec.md`](docs/guidance-runner-spec.md) | The session, flow and invariant-register workstreams: what each delivers, the seam every borrowed part sits behind, and the corrections two research sweeps made to the original design. |
| [`docs/estate-asset-inventory.md`](docs/estate-asset-inventory.md) | The adjacent repositories searched before any of it was written, what was taken, and what was deliberately left behind. |
| [`docs/frontier.md`](docs/frontier.md) | The state of the art across the nine areas this bot competes in, with measured anchors and the design decision each forces. |
| [`docs/framework-comparison.md`](docs/framework-comparison.md) | Why the field looks the same, the four axes it actually differs on, and the axis nobody occupies. |
| [`docs/releasing.md`](docs/releasing.md) | The release process. |
| [`CHANGELOG.md`](CHANGELOG.md) | Per-release changes across all four crates. |
| [`SECURITY.md`](SECURITY.md) | Attack surface, reporting process, and advisories assessed. |

Each crate also carries its own README: [`lgwks_std`](crates/lgwks-std/README.md),
[`lgwks_bot`](crates/lgwks-bot/README.md),
[`lgwks_ast`](crates/lgwks-ast/README.md),
[`lgwks_deps`](crates/lgwks-deps/README.md).

## Building and testing

```sh
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo run -p lgwks_deps -- check .
./scripts/lgwks-std-package-smoke.sh
```

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). Report vulnerabilities through the
process in [`SECURITY.md`](SECURITY.md), not as a public issue.

## License

Copyright 2026 Logical Works Incorporated. Each crate ships its own `LICENSE`
file.

`lgwks_std`, `lgwks_ast`, and `lgwks_deps` are Apache-2.0, for example
[`crates/lgwks-std/LICENSE`](crates/lgwks-std/LICENSE). `lgwks_bot` is
[MPL-2.0](crates/lgwks-bot/LICENSE) — file-level copyleft rather than
permissive, so that the artefact the whole estate embeds cannot be taken,
modified, and shipped closed.

Both permit proprietary *use*; the difference is what happens when someone
modifies the covered files. [`LICENSING.md`](LICENSING.md) states the rule, why
the bot differs, and the commercial licence available for the one case MPL-2.0
does not cover.
